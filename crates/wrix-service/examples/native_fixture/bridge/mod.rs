//! Fixed guest peer for the Linux bridge backpressure acceptance boundary.

use std::{
    io::{self, Read, Write},
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// native bridge fixture I/O failed: {0}
    Io(#[from] io::Error),
    /// unexpected bridge peer magic
    Magic,
    /// unexpected bridge data frame
    Frame,
    /// unexpected bridge data payload
    Payload,
    /// native pipe capacity is not 4096 bytes
    Capacity,
    /// usage: wrix-native-fixtures bridge-peer
    Usage,
}

const MAGIC: &[u8; 8] = b"WRIXNB01";
const CHUNK: usize = 16_384;

fn frame(kind: u8, stream: u32, size: u32, output: &mut impl Write) -> Result<(), Error> {
    output.write_all(&[kind])?;
    output.write_all(&stream.to_be_bytes())?;
    output.write_all(&size.to_be_bytes())?;
    output.flush()?;
    Ok(())
}

fn peer(
    input: &mut impl Read,
    output: &mut impl Write,
    mut pause: impl FnMut(Duration),
) -> Result<(), Error> {
    output.write_all(MAGIC)?;
    output.flush()?;
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(Error::Magic);
    }
    frame(1, 1, 0, output)?;
    pause(Duration::from_secs(31));
    let mut header = [0; 9];
    input.read_exact(&mut header)?;
    if header != [2, 0, 0, 0, 1, 0, 0, 64, 0] {
        return Err(Error::Frame);
    }
    let mut payload = [0; CHUNK];
    input.read_exact(&mut payload)?;
    if payload != [b'x'; CHUNK] {
        return Err(Error::Payload);
    }
    frame(4, 1, 0, output)?;
    frame(3, 1, 0, output)?;
    frame(1, 2, 0, output)?;
    pause(Duration::from_secs(120));
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn run(args: &[std::ffi::OsString]) -> Result<std::process::ExitCode, Error> {
    if !args.is_empty() {
        return Err(Error::Usage);
    }
    let input = io::stdin();
    if rustix::pipe::fcntl_setpipe_size(&input, 4096).map_err(io::Error::from)? != 4096
        || rustix::pipe::fcntl_getpipe_size(&input).map_err(io::Error::from)? != 4096
    {
        return Err(Error::Capacity);
    }
    peer(
        &mut input.lock(),
        &mut io::stdout().lock(),
        std::thread::sleep,
    )?;
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod test {
    use super::*;

    fn input() -> Vec<u8> {
        let mut input = MAGIC.to_vec();
        input.extend_from_slice(&[2, 0, 0, 0, 1, 0, 0, 64, 0]);
        input.extend_from_slice(&[b'x'; CHUNK]);
        input
    }

    #[test]
    fn peer_preserves_exact_frames_credit_order_and_original_deadlines() {
        let mut output = Vec::new();
        let mut pauses = Vec::new();
        peer(&mut input().as_slice(), &mut output, |delay| {
            pauses.push(delay)
        })
        .unwrap();
        let expected = [
            MAGIC.as_slice(),
            &[1, 0, 0, 0, 1, 0, 0, 0, 0],
            &[4, 0, 0, 0, 1, 0, 0, 0, 0],
            &[3, 0, 0, 0, 1, 0, 0, 0, 0],
            &[1, 0, 0, 0, 2, 0, 0, 0, 0],
        ]
        .concat();
        assert_eq!(output, expected);
        assert_eq!(pauses, [Duration::from_secs(31), Duration::from_secs(120)]);
    }

    #[test]
    fn wrong_magic_frame_payload_and_short_transport_fail_closed() {
        for (position, error) in [(0, "magic"), (8, "frame"), (17, "payload")] {
            let mut input = input();
            input[position] ^= 1;
            let result = peer(&mut input.as_slice(), &mut Vec::new(), |_| {});
            assert!(result.unwrap_err().to_string().contains(error));
        }
        assert!(peer(&mut &input()[..20], &mut Vec::new(), |_| {}).is_err());
    }
}
