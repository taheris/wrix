//! Private passive host-policy/metadata helper; never publishes or provisions state.

use std::{
    io::{self, Read, Write},
    path::PathBuf,
    process::ExitCode,
};
use wrix_cache::host_policy::Inventory;

fn input() -> Result<String, Box<dyn std::error::Error>> {
    let mut text = String::new();
    io::stdin().take(1_048_577).read_to_string(&mut text)?;
    if text.len() > 1_048_576 {
        return Err("host metadata exceeds the inspection limit".into());
    }
    Ok(text)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let operation = arguments.next().ok_or("missing private helper operation")?;
    match operation.to_str() {
        Some("json-string" | "json-boolean") => {
            let key = arguments.next().ok_or("missing metadata field")?;
            if arguments.next().is_some() {
                return Err("unexpected metadata arguments".into());
            }
            let key = key.to_str().ok_or("invalid metadata field encoding")?;
            let value: serde_json::Value = serde_json::from_str(&input()?)?;
            let field = value.get(key).ok_or("missing metadata field")?;
            if operation == "json-boolean" {
                writeln!(
                    io::stdout().lock(),
                    "{}",
                    field.as_bool().ok_or("invalid boolean metadata")?
                )?;
            } else {
                let text = field
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or("invalid string metadata")?;
                writeln!(io::stdout().lock(), "{text}")?;
            }
        }
        Some("cache-endpoint") => {
            if arguments.next().is_some() {
                return Err("unexpected endpoint arguments".into());
            }
            if let Some(endpoint) = wrix_cache::host_policy::cache_endpoint(&input()?)? {
                writeln!(io::stdout().lock(), "{endpoint}")?;
            }
        }
        Some("inspect") => {
            let state = PathBuf::from(arguments.next().ok_or("missing private state path")?);
            let cache = PathBuf::from(arguments.next().ok_or("missing private cache path")?);
            let endpoint = arguments.next().ok_or("missing endpoint")?;
            if arguments.next().is_some() {
                return Err("unexpected policy-helper arguments".into());
            }
            let endpoint = endpoint.to_str().ok_or("invalid endpoint encoding")?;
            Inventory::read(&state, &cache, endpoint)?.check(&input()?)?;
        }
        _ => return Err("unknown private helper operation".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "wrix: publication-only host cache policy rejected activation: {error}"
            );
            ExitCode::FAILURE
        }
    }
}
