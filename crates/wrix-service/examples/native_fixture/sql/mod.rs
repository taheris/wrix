use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    net::SocketAddr,
    os::unix::net::UnixStream,
    path::Path,
    process::ExitCode,
    time::Duration,
};
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// unsupported native SQL fixture arguments
    Usage,
    /// native SQL fixture environment is missing or invalid: {0}
    Environment(&'static str),
    /// native SQL fixture I/O failed: {0}
    Io(#[from] io::Error),
    /// native SQL fixture JSON failed: {0}
    Json(#[from] serde_json::Error),
    /// native SQL control frame is truncated or exceeds 4096 bytes
    Frame,
    /// native SQL control request was rejected
    Rejected,
}

fn text(value: &OsString) -> Result<&str, Error> {
    value.to_str().ok_or(Error::Usage)
}

fn request(args: &[OsString], gateway_key: Option<String>) -> Result<Value, Error> {
    match args {
        [] => Ok(json!({"command":"abort"})),
        [mode] if mode == "compiler-ready" => Ok(json!({"role":"compiler-ready"})),
        [mode, listen, target] if mode == "forward" => {
            let listen: SocketAddr = text(listen)?.parse().map_err(|_| Error::Usage)?;
            let target: SocketAddr = text(target)?.parse().map_err(|_| Error::Usage)?;
            Ok(json!({"role":"forward","listen":listen.to_string(),"target":target.to_string()}))
        }
        [mode, rest @ ..] if mode == "gateway" => {
            let mut ports = BTreeMap::new();
            for (index, arg) in rest.iter().enumerate() {
                if arg == "-p" {
                    let value = text(rest.get(index + 1).ok_or(Error::Usage)?)?;
                    let (listen, port) = value.rsplit_once(':').ok_or(Error::Usage)?;
                    let port: u16 = port.parse().map_err(|_| Error::Usage)?;
                    let listen: SocketAddr = listen.parse().map_err(|_| Error::Usage)?;
                    ports.insert(port, listen);
                }
            }
            let data = ports.get(&3128).ok_or(Error::Usage)?.to_string();
            let management = ports.get(&3129).ok_or(Error::Usage)?.to_string();
            let key = gateway_key.ok_or(Error::Environment("WRIX_GATEWAY_KEY"))?;
            Ok(json!({"role":"gateway","data":data,"management":management,"key":key}))
        }
        _ => Err(Error::Usage),
    }
}

fn exchange(path: &Path, request: &Value) -> Result<Vec<u8>, Error> {
    let mut client = UnixStream::connect(path)?;
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    client.set_write_timeout(Some(Duration::from_secs(30)))?;
    serde_json::to_writer(&mut client, request)?;
    client.write_all(b"\n")?;
    let mut response = Vec::with_capacity(4096);
    let mut buffer = [0_u8; 4097];
    while response.last() != Some(&b'\n') {
        let remaining = 4097 - response.len();
        let length = client.read(&mut buffer[..remaining])?;
        if length == 0 || response.len() + length > 4096 {
            return Err(Error::Frame);
        }
        response.extend_from_slice(&buffer[..length]);
    }
    let reply: Value = serde_json::from_slice(&response)?;
    if reply.get("error").is_some() {
        return Err(Error::Rejected);
    }
    Ok(response)
}

pub(super) fn run(args: &[OsString]) -> Result<ExitCode, Error> {
    let Some((socket, args)) = args.split_first() else {
        return Err(Error::Usage);
    };
    let key = if args.first().is_some_and(|mode| mode == "gateway") {
        Some(
            std::env::var("WRIX_GATEWAY_KEY")
                .map_err(|_| Error::Environment("WRIX_GATEWAY_KEY"))?,
        )
    } else {
        None
    };
    let response = exchange(Path::new(socket), &request(args, key)?)?;
    io::stdout().lock().write_all(&response)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::{io::BufRead, os::unix::net::UnixListener};

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn native_sql_control_requests_preserve_all_role_shapes() {
        assert_eq!(request(&[], None).unwrap(), json!({"command":"abort"}));
        assert_eq!(
            request(&args(&["compiler-ready"]), None).unwrap(),
            json!({"role":"compiler-ready"})
        );
        assert_eq!(
            request(
                &args(&["forward", "127.0.0.1:1000", "127.0.0.1:3306"]),
                None
            )
            .unwrap(),
            json!({"role":"forward","listen":"127.0.0.1:1000","target":"127.0.0.1:3306"})
        );
        assert_eq!(
            request(
                &args(&[
                    "gateway",
                    "run",
                    "-p",
                    "127.0.0.1:1000:3128",
                    "-p",
                    "127.0.0.1:1001:3129"
                ]),
                Some("fixture-key".to_owned())
            )
            .unwrap(),
            json!({"role":"gateway","data":"127.0.0.1:1000","management":"127.0.0.1:1001","key":"fixture-key"})
        );
    }

    #[test]
    fn malformed_roles_addresses_and_missing_gateway_ports_fail_closed() {
        for values in [
            vec!["unknown"],
            vec!["forward", "invalid", "127.0.0.1:3306"],
            vec!["gateway", "-p"],
            vec!["gateway", "-p", "invalid"],
            vec!["compiler-ready", "extra"],
        ] {
            assert!(request(&args(&values), None).is_err());
        }
    }

    fn reply(bytes: Vec<u8>) -> Result<Vec<u8>, Error> {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut request)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap(),
                json!({"command":"abort"})
            );
            stream.write_all(&bytes).unwrap();
        });
        let result = exchange(&socket, &json!({"command":"abort"}));
        server.join().unwrap();
        result
    }

    #[test]
    fn native_sql_client_exchanges_with_an_actual_owned_unix_server() {
        assert_eq!(
            reply(b"{\"ready\":true}\n".to_vec()).unwrap(),
            b"{\"ready\":true}\n"
        );
        let mut maximum = b"{}".to_vec();
        maximum.resize(4095, b' ');
        maximum.push(b'\n');
        assert_eq!(reply(maximum).unwrap().len(), 4096);
    }

    #[test]
    fn native_sql_client_rejects_oversize_truncated_malformed_and_error_frames() {
        assert!(matches!(reply(vec![b'x'; 4097]), Err(Error::Frame)));
        assert!(matches!(reply(b"{}".to_vec()), Err(Error::Frame)));
        assert!(matches!(reply(b"not-json\n".to_vec()), Err(Error::Json(_))));
        assert!(matches!(
            reply(b"{\"error\":\"fixture rejection\"}\n".to_vec()),
            Err(Error::Rejected)
        ));
    }
}
