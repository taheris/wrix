# Keep dockerTools' archive format, but make its threaded layer pipe fail closed.
{ pkgs }:
let
  inherit (pkgs) lib;
  baseDockerTools = import ./docker-tools.nix { inherit pkgs; };
  upstreamSource = pkgs.path + "/pkgs/build-support/docker/stream_layered_image.py";
  indent =
    text:
    lib.concatMapStringsSep "\n" (line: if line == "" then "" else "    " + line) (
      lib.splitString "\n" text
    );
  old = indent ''
    read_fd, write_fd = os.pipe()
    with open(read_fd, "rb") as read, open(write_fd, "wb") as write:

        def producer():
            archive_paths_to(write, paths, mtime, uid, gid, uname, gname)
            write.close()

        # Closing the write end of the fifo also closes the read end,
        # so we don't need to wait until this thread is finished.
        #
        # Any exception from the thread will get printed by the default
        # exception handler, and the 'addfile' call will fail since it
        # won't be able to read required amount of bytes.
        threading.Thread(target=producer).start()
        tar.addfile(layer_tarinfo, read)
  '';
  replacement = indent ''
    read_fd, write_fd = os.pipe()
    failures = []

    def producer():
        try:
            with os.fdopen(write_fd, "wb") as write:
                archive_paths_to(write, paths, mtime, uid, gid, uname, gname)
        except BaseException as error:
            failures.append(error)

    worker = threading.Thread(target=producer)
    consumer_error = None
    with os.fdopen(read_fd, "rb") as read:
        worker.start()
        try:
            tar.addfile(layer_tarinfo, read)
        except BaseException as error:
            consumer_error = error
        finally:
            # Only the producer owns/closes the buffered writer. Closing it
            # here can deadlock on its lock while that thread waits for us.
            read.close()
            worker.join()
    if consumer_error is not None:
        if failures and str(consumer_error) == "unexpected end of data":
            raise failures[0] from consumer_error
        raise consumer_error
    if failures:
        raise failures[0]
  '';
  source = builtins.readFile upstreamSource;
  patchedSource =
    assert lib.assertMsg (
      builtins.length (lib.splitString old source) == 2
    ) "dockerTools layer-stream implementation changed; review the pipe fix";
    pkgs.writeText "wrix-image-stream.py" (lib.replaceStrings [ old ] [ replacement ] source);
  program = pkgs.writers.writePython3 "wrix-image-stream" { } patchedSource;
  replaceStream =
    from: to: command:
    let
      bareFrom = builtins.unsafeDiscardStringContext (toString from);
      bareCommand = builtins.unsafeDiscardStringContext command;
      remaining = builtins.removeAttrs (builtins.getContext command) (
        builtins.attrNames (builtins.getContext (toString from))
      );
      text =
        lib.replaceStrings [ bareFrom ] [ (builtins.unsafeDiscardStringContext (toString to)) ]
          bareCommand;
    in
    assert lib.assertMsg (
      builtins.length (lib.splitString bareFrom bareCommand) == 2
    ) "dockerTools compression command changed; review the stream replacement";
    builtins.appendContext text (remaining // builtins.getContext (toString to));
  patchStream =
    stream:
    stream.overrideAttrs (previous: {
      streamScript = program;
      passthru = (previous.passthru or { }) // {
        streamScript = program;
      };
    });
in
{
  inherit upstreamSource patchedSource;
  dockerTools = baseDockerTools // {
    streamLayeredImage = lib.makeOverridable (
      args: patchStream (baseDockerTools.streamLayeredImage args)
    );
    buildLayeredImage = lib.makeOverridable (
      args:
      let
        image = baseDockerTools.buildLayeredImage args;
        stream = patchStream image.stream;
      in
      image.overrideAttrs (previous: {
        buildCommand = replaceStream image.stream stream previous.buildCommand;
        passthru = (previous.passthru or { }) // {
          inherit stream;
        };
      })
    );
  };
}
