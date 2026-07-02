use std::{
    env, fs,
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};

use wrix_core::path::WorkspaceHash;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Helper {
    Hook,
    Publish,
    Serve,
}

#[derive(Clone, Debug)]
struct HookConfig {
    workspace_hash: String,
    owner_uid: u32,
    owner_gid: u32,
    state_root: PathBuf,
    cache_root: PathBuf,
    manifest: PathBuf,
    publisher_helper: PathBuf,
}

#[derive(Clone, Debug)]
struct PublishConfig {
    workspace_hash: String,
    state_root: PathBuf,
    cache_root: PathBuf,
    manifest: PathBuf,
    drv_path: String,
    out_paths: String,
}

impl Helper {
    const fn binary_name(self) -> &'static str {
        match self {
            Self::Hook => "wrix-cache-hook",
            Self::Publish => "wrix-cache-publish",
            Self::Serve => "wrix-cache-serve",
        }
    }

    const fn purpose(self) -> &'static str {
        match self {
            Self::Hook => "Run the project cache post-build hook.",
            Self::Publish => "Publish project cache paths.",
            Self::Serve => "Serve the project cache over HTTP.",
        }
    }
}

pub fn main(helper: Helper) -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match run(helper, &args, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            if writeln!(stderr, "{}: {error}", helper.binary_name()).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

fn run(
    helper: Helper,
    args: &[String],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    if args.is_empty()
        || args
            .first()
            .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        write_help(helper, stdout)?;
        return Ok(ExitCode::SUCCESS);
    }
    match helper {
        Helper::Hook => run_hook(args, stdout),
        Helper::Publish => run_publish(args, stdout),
        Helper::Serve => run_serve(args, stderr),
    }
}

fn run_serve(args: &[String], stderr: &mut impl Write) -> io::Result<ExitCode> {
    let (listen, root) = match args {
        [root] => ("0.0.0.0:8080", root.as_str()),
        [flag, listen, root] if flag == "--listen" => (listen.as_str(), root.as_str()),
        _ => {
            writeln!(
                stderr,
                "Usage: wrix-cache-serve [--listen HOST:PORT] <cache-root>"
            )?;
            return Ok(ExitCode::FAILURE);
        }
    };
    let root = PathBuf::from(root);
    require_absolute_dir("cache root", &root)?;
    let listener = TcpListener::bind(listen)?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_cache_request(stream, &root)?,
            Err(error) => writeln!(stderr, "wrix-cache-serve: accept failed: {error}")?,
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn handle_cache_request(stream: TcpStream, root: &Path) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let mut stream = stream;
    match method {
        "GET" => serve_cache_path(&mut stream, root, target, true),
        "HEAD" => serve_cache_path(&mut stream, root, target, false),
        _ => write_response(
            &mut stream,
            "405 Method Not Allowed",
            b"method not allowed\n",
            true,
        ),
    }
}

fn serve_cache_path(
    stream: &mut TcpStream,
    root: &Path,
    target: &str,
    include_body: bool,
) -> io::Result<()> {
    let Some(relative) = parse_cache_target(target) else {
        return write_response(stream, "404 Not Found", b"not found\n", include_body);
    };
    let Some(path) = resolve_cache_file(root, relative)? else {
        return write_response(stream, "404 Not Found", b"not found\n", include_body);
    };
    let body = fs::read(path)?;
    write_response(stream, "200 OK", &body, include_body)
}

fn resolve_cache_file(root: &Path, relative: &str) -> io::Result<Option<PathBuf>> {
    let root = root.canonicalize()?;
    let path = root.join(relative);
    let path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if path.starts_with(&root) && path.is_file() {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

fn parse_cache_target(target: &str) -> Option<&str> {
    let path = target.split('?').next()?.trim_start_matches('/');
    if path.is_empty() || path.contains('\\') || has_forbidden_segment(path) {
        return None;
    }
    if path == "nix-cache-info"
        || is_root_narinfo(path)
        || path.starts_with("nar/")
        || path.starts_with("log/")
    {
        Some(path)
    } else {
        None
    }
}

fn has_forbidden_segment(path: &str) -> bool {
    path.split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
}

fn is_root_narinfo(path: &str) -> bool {
    path.ends_with(".narinfo") && !path.contains('/')
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
    include_body: bool,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if include_body {
        stream.write_all(body)?;
    }
    Ok(())
}

fn run_hook(args: &[String], stdout: &mut impl Write) -> io::Result<ExitCode> {
    let config = HookConfig::parse(args)?;
    config.validate()?;
    let drv_path = env::var("DRV_PATH").map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "DRV_PATH is required for the post-build hook",
        )
    })?;
    let out_paths = env::var("OUT_PATHS").unwrap_or_else(|_| String::new());
    let manifest = fs::read_to_string(&config.manifest)?;
    if !manifest_allows_drv(&manifest, &drv_path) {
        writeln!(
            stdout,
            "wrix-cache-hook: skipping non-project derivation {drv_path}"
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    exec_publisher(config, drv_path, out_paths)
}

fn run_publish(args: &[String], stdout: &mut impl Write) -> io::Result<ExitCode> {
    let config = PublishConfig::parse(args)?;
    let report = crate::publisher::run_hook_record(
        &config.workspace_hash,
        &config.state_root,
        &config.cache_root,
        &config.manifest,
        &config.drv_path,
        &config.out_paths,
    )?;
    for line in report.lines() {
        writeln!(stdout, "{line}")?;
    }
    Ok(ExitCode::SUCCESS)
}

fn exec_publisher(config: HookConfig, drv_path: String, out_paths: String) -> io::Result<ExitCode> {
    let current_uid = current_uid()?;
    if current_uid != 0 && current_uid != config.owner_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "hook is running as uid {current_uid}, expected root or workspace owner {}",
                config.owner_uid
            ),
        ));
    }
    let mut command = Command::new(&config.publisher_helper);
    command
        .arg("--workspace-hash")
        .arg(config.workspace_hash)
        .arg("--state-root")
        .arg(config.state_root)
        .arg("--cache-root")
        .arg(config.cache_root)
        .arg("--manifest")
        .arg(config.manifest)
        .arg("--drv-path")
        .arg(drv_path)
        .arg("--out-paths")
        .arg(out_paths)
        .stdin(Stdio::null());
    #[cfg(unix)]
    if current_uid == 0 {
        command.gid(config.owner_gid).uid(config.owner_uid);
    }
    exec_command(command)
}

fn exec_command(mut command: Command) -> io::Result<ExitCode> {
    #[cfg(unix)]
    {
        let error = command.exec();
        Err(error)
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }
}

fn current_uid() -> io::Result<u32> {
    let output = Command::new("id")
        .arg("-u")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    parse_u32(
        String::from_utf8_lossy(&output.stdout).trim(),
        "current uid",
    )
}

impl HookConfig {
    fn parse(args: &[String]) -> io::Result<Self> {
        let mut parser = FlagParser::new(args);
        let workspace_hash = parser.required("--workspace-hash")?;
        let owner_user_id = parse_u32(&parser.required("--owner-uid")?, "owner uid")?;
        let owner_group_id = parse_u32(&parser.required("--owner-gid")?, "owner gid")?;
        let state_root = PathBuf::from(parser.required("--state-root")?);
        let cache_root = PathBuf::from(parser.required("--cache-root")?);
        let manifest = PathBuf::from(parser.required("--manifest")?);
        let publisher_helper = PathBuf::from(parser.required("--publisher-helper")?);
        parser.finish()?;
        Ok(Self {
            workspace_hash,
            owner_uid: owner_user_id,
            owner_gid: owner_group_id,
            state_root,
            cache_root,
            manifest,
            publisher_helper,
        })
    }

    fn validate(&self) -> io::Result<()> {
        validate_workspace_hash(&self.workspace_hash)?;
        require_absolute_dir("state root", &self.state_root)?;
        require_absolute_dir("cache root", &self.cache_root)?;
        require_absolute_file("publish manifest", &self.manifest)?;
        require_executable_file("publisher helper", &self.publisher_helper)
    }
}

impl PublishConfig {
    fn parse(args: &[String]) -> io::Result<Self> {
        let mut parser = FlagParser::new(args);
        let workspace_hash = parser.required("--workspace-hash")?;
        let state_root = PathBuf::from(parser.required("--state-root")?);
        let cache_root = PathBuf::from(parser.required("--cache-root")?);
        let manifest = PathBuf::from(parser.required("--manifest")?);
        let drv_path = parser.required("--drv-path")?;
        let out_paths = parser.required("--out-paths")?;
        parser.finish()?;
        Ok(Self {
            workspace_hash,
            state_root,
            cache_root,
            manifest,
            drv_path,
            out_paths,
        })
    }
}

struct FlagParser<'a> {
    args: &'a [String],
    index: usize,
}

impl<'a> FlagParser<'a> {
    const fn new(args: &'a [String]) -> Self {
        Self { args, index: 0 }
    }

    fn required(&mut self, name: &str) -> io::Result<String> {
        if self.args.get(self.index).map(String::as_str) != Some(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("expected {name}"),
            ));
        }
        self.index += 1;
        let value = self.args.get(self.index).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("missing value for {name}"),
            )
        })?;
        self.index += 1;
        Ok(value)
    }

    fn finish(&self) -> io::Result<()> {
        if self.index == self.args.len() {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unexpected argument {}", self.args[self.index]),
        ))
    }
}

fn validate_workspace_hash(hash: &str) -> io::Result<()> {
    if WorkspaceHash::is_valid_str(hash) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "workspace hash must be a lowercase sha256 hex digest",
    ))
}

fn require_absolute_dir(label: &str, path: &Path) -> io::Result<()> {
    if path.is_absolute() && path.is_dir() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{label} must be an absolute directory: {}", path.display()),
    ))
}

fn require_absolute_file(label: &str, path: &Path) -> io::Result<()> {
    if path.is_absolute() && path.is_file() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{label} must be an absolute file: {}", path.display()),
    ))
}

fn require_executable_file(label: &str, path: &Path) -> io::Result<()> {
    require_absolute_file(label, path)?;
    #[cfg(unix)]
    {
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{label} is not executable: {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn manifest_allows_drv(manifest: &str, drv_path: &str) -> bool {
    manifest_key_values(manifest)
        .into_iter()
        .any(|(key, value)| matches!(key.as_str(), "drv_path" | "drvPath") && value == drv_path)
}

fn manifest_key_values(input: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut rest = input;
    while let Some(key_start) = rest.find('"') {
        let after_key_start = &rest[key_start + 1..];
        let Some(key_end) = after_key_start.find('"') else {
            break;
        };
        let key = &after_key_start[..key_end];
        let after_key = &after_key_start[key_end + 1..];
        let Some(colon) = after_key.find(':') else {
            rest = after_key;
            continue;
        };
        let after_colon = after_key[colon + 1..].trim_start();
        if let Some(value_start) = after_colon.strip_prefix('"')
            && let Some(value_end) = value_start.find('"')
        {
            pairs.push((key.to_owned(), value_start[..value_end].to_owned()));
            rest = &value_start[value_end + 1..];
            continue;
        }
        rest = after_colon;
    }
    pairs
}

fn parse_u32(input: &str, label: &str) -> io::Result<u32> {
    input.parse::<u32>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {label} {input}: {error}"),
        )
    })
}

fn write_help(helper: Helper, stdout: &mut impl Write) -> io::Result<()> {
    match helper {
        Helper::Hook => writeln!(
            stdout,
            "{}\n\nUsage: {} --workspace-hash HASH --owner-uid UID --owner-gid GID --state-root PATH --cache-root PATH --manifest PATH --publisher-helper PATH",
            helper.purpose(),
            helper.binary_name()
        ),
        Helper::Publish => writeln!(
            stdout,
            "{}\n\nUsage: {} --workspace-hash HASH --state-root PATH --cache-root PATH --manifest PATH --drv-path PATH --out-paths PATHS",
            helper.purpose(),
            helper.binary_name()
        ),
        Helper::Serve => writeln!(
            stdout,
            "{}\n\nUsage: {} [--listen HOST:PORT] <cache-root>",
            helper.purpose(),
            helper.binary_name()
        ),
    }
}

#[cfg(test)]
mod test {
    use std::{
        fs,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        path::Path,
        thread,
    };

    use super::{
        handle_cache_request, manifest_allows_drv, manifest_key_values, parse_cache_target,
    };

    #[test]
    fn manifest_scope_accepts_matching_drv_path() {
        let manifest = r#"{"roots":[{"name":"pkg","drv_path":"/nix/store/aaa.drv"}]}"#;
        assert!(manifest_allows_drv(manifest, "/nix/store/aaa.drv"));
        assert!(!manifest_allows_drv(manifest, "/nix/store/bbb.drv"));
    }

    #[test]
    fn manifest_parser_extracts_string_pairs() {
        let pairs = manifest_key_values(r#"{"drvPath":"/nix/store/aaa.drv"}"#);
        assert_eq!(
            pairs,
            vec![(String::from("drvPath"), String::from("/nix/store/aaa.drv"))]
        );
    }

    #[test]
    fn cache_server_accepts_only_nix_cache_paths() {
        assert_eq!(
            parse_cache_target("/nix-cache-info"),
            Some("nix-cache-info")
        );
        assert_eq!(parse_cache_target("/abc.narinfo"), Some("abc.narinfo"));
        assert_eq!(parse_cache_target("/nar/abc.nar"), Some("nar/abc.nar"));
        assert_eq!(parse_cache_target("/log/abc"), Some("log/abc"));
        assert_eq!(parse_cache_target("/"), None);
        assert_eq!(parse_cache_target("/nar/../secret"), None);
        assert_eq!(parse_cache_target("/nar//secret"), None);
        assert_eq!(parse_cache_target("/nested/abc.narinfo"), None);
        assert_eq!(parse_cache_target("/index.html"), None);
    }

    #[test]
    fn cache_server_returns_get_body_for_allowed_path() {
        let root = temp_cache_root("get-body");
        fs::write(root.join("nix-cache-info"), "StoreDir: /nix/store\n").unwrap();
        let response = request(&root, "GET /nix-cache-info HTTP/1.1\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with("StoreDir: /nix/store\n"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_server_head_omits_body() {
        let root = temp_cache_root("head-no-body");
        fs::write(root.join("abc.narinfo"), "StorePath: /nix/store/abc\n").unwrap();
        let response = request(&root, "HEAD /abc.narinfo HTTP/1.1\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        assert!(!response.contains("StorePath"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_server_rejects_write_methods_and_traversal() {
        let root = temp_cache_root("rejects");
        fs::create_dir_all(root.join("nar")).unwrap();
        fs::write(root.join("secret"), "secret\n").unwrap();
        fs::write(root.join("nar/good.nar"), "nar\n").unwrap();
        let method = request(&root, "POST /nar/good.nar HTTP/1.1\r\n\r\n");
        let traversal = request(&root, "GET /nar/../secret HTTP/1.1\r\n\r\n");
        assert!(method.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(traversal.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(!traversal.contains("secret"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cache_server_rejects_symlink_escape() {
        let root = temp_cache_root("symlink-escape");
        let outside = root.with_extension("outside-secret");
        fs::create_dir_all(root.join("nar")).unwrap();
        fs::write(&outside, "secret\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("nar/link.nar")).unwrap();
        let response = request(&root, "GET /nar/link.nar HTTP/1.1\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(!response.contains("secret"));
        fs::remove_file(outside).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn request(root: &Path, request: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let root = root.to_path_buf();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_cache_request(stream, &root).unwrap();
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        handle.join().unwrap();
        response
    }

    fn temp_cache_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("wrix-cache-serve-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }
}
