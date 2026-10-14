#![cfg(test)]

use std::{
    collections::HashMap,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use tempfile::TempDir;
use wrix_notify::{
    attention,
    navigation::Navigator,
    process::{Error, Runner},
    route::{Platform, Route, Target},
};

#[derive(Default)]
pub struct Desktop {
    pub windows: Value,
    pub calls: Vec<(String, Vec<String>)>,
    pub failures: Vec<Vec<String>>,
    pub responses: HashMap<Vec<String>, String>,
    pub ax: Option<String>,
}

#[derive(Clone)]
pub struct Stub(pub Arc<Mutex<Desktop>>);

impl Runner for Stub {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, Error> {
        let mut state = self.0.lock().unwrap();
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        state.calls.push((program.to_owned(), args.clone()));
        if state.failures.contains(&args) {
            return Err(Error::Timeout);
        }
        if let Some(response) = state.responses.get(&args) {
            return Ok(response.clone());
        }
        match (
            program,
            args.iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice(),
        ) {
            ("niri", ["msg", "-j", "windows"]) => Ok(state.windows.to_string()),
            ("niri", ["msg", "action", "focus-window", "--id", id]) => {
                for window in state.windows.as_array_mut().unwrap() {
                    window["is_focused"] = json!(window["id"].to_string().trim_matches('"') == *id);
                }
                Ok(String::new())
            }
            ("osascript", ["-e", script, _, _]) => Ok(state.ax.clone().unwrap_or_else(|| {
                if script.contains("AXRaise") {
                    "success"
                } else {
                    "true"
                }
                .to_owned()
            })),
            ("tmux", [_, _, "list-clients", ..]) => Ok(String::new()),
            _ => Err(Error::Io(std::io::Error::other(format!(
                "unconfigured fixture: {program} {args:?}"
            )))),
        }
    }
}

pub struct Fixture {
    pub root: TempDir,
    pub directory: PathBuf,
    pub state: Arc<Mutex<Desktop>>,
    pub navigator: Navigator<Stub>,
    pub platform: Platform,
}

impl Fixture {
    pub fn new(platform: Platform) -> Self {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("sessions");
        fs::create_dir(&directory).unwrap();
        let proc = root.path().join("proc");
        fs::create_dir_all(proc.join("net")).unwrap();
        fs::write(
            proc.join("net/unix"),
            "Num RefCount Protocol Flags Type St Inode Path\n",
        )
        .unwrap();
        symlink(root.path(), proc.join("self")).unwrap();
        let state = Arc::new(Mutex::new(Desktop {
            windows: json!([
                {"id":1, "pid":10, "is_focused":false},
                {"id":2, "pid":20, "is_focused":false},
                {"id":99, "pid":99, "is_focused":true}
            ]),
            ..Desktop::default()
        }));
        let navigator = Navigator::new(Stub(state.clone()), proc);
        Self {
            root,
            directory,
            state,
            navigator,
            platform,
        }
    }

    pub fn linux() -> Self {
        Self::new(Platform::Linux)
    }

    pub fn register(&self, target: &str, id: u32) -> PathBuf {
        let path = self.directory.join(Self::target(target).filename());
        fs::write(
            &path,
            json!({"focus_target": target, "window_id": id.to_string(),
            "linux_route":{"terminal_pid":id * 10, "client":null}})
            .to_string(),
        )
        .unwrap();
        path
    }

    pub fn mac(&self, target: &str, title: &str) -> PathBuf {
        let path = self.directory.join(Self::target(target).filename());
        fs::write(
            &path,
            json!({"focus_target":target, "terminal_app":"Alacritty",
            "terminal_window_title":title})
            .to_string(),
        )
        .unwrap();
        path
    }

    pub fn target(text: &str) -> Target {
        text.to_owned().try_into().unwrap()
    }

    pub fn enqueue(&self, target: &str) {
        attention::enqueue(&self.directory, &Self::target(target), self.platform).unwrap();
    }

    pub fn focus(&self) {
        attention::focus(&self.directory, self.platform, &self.navigator).unwrap();
    }
    pub fn returning(&self) {
        attention::return_to_origin(&self.directory, self.platform, &self.navigator).unwrap();
    }
    pub fn focused(&self, target: &str) -> bool {
        attention::is_focused(
            &self.directory,
            &Self::target(target),
            self.platform,
            &self.navigator,
        )
    }

    pub fn pending(&self) -> Vec<String> {
        attention::open(&self.directory)
            .unwrap()
            .prepare("SELECT target FROM pending ORDER BY sequence DESC")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub fn saved(&self) -> Option<Value> {
        use rusqlite::OptionalExtension;
        attention::open(&self.directory)
            .unwrap()
            .query_row("SELECT route FROM return_position", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .unwrap()
            .map(|text| serde_json::from_str(&text).unwrap())
    }

    pub fn windows(&self, value: Value) {
        self.state.lock().unwrap().windows = value;
    }
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.state.lock().unwrap().calls.clone()
    }
    pub fn visits(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|(program, args)| {
                (program == "niri" && args.get(1).is_some_and(|s| s == "action"))
                    .then(|| args[4].clone())
            })
            .collect()
    }

    pub fn fail_focus(&self, id: &str) {
        self.state.lock().unwrap().failures.push(vec![
            "msg".into(),
            "action".into(),
            "focus-window".into(),
            "--id".into(),
            id.into(),
        ]);
    }

    pub fn response(&self, args: &[&str], value: &str) {
        self.state.lock().unwrap().responses.insert(
            args.iter().map(|s| (*s).to_owned()).collect(),
            value.to_owned(),
        );
    }

    pub fn fake_process(&self, pid: u32, parent: u32, tty: Option<&str>) {
        let path = self.root.path().join("proc").join(pid.to_string());
        fs::create_dir_all(path.join("fd")).unwrap();
        fs::write(path.join("status"), format!("PPid:\t{parent}\n")).unwrap();
        if let Some(tty) = tty {
            symlink(tty, path.join("fd/0")).unwrap();
        }
    }

    pub fn tmux_route(&self) -> Route {
        self.fake_process(11, 10, Some("/dev/pts/1"));
        self.fake_process(10, 1, None);
        Route::parse(
            &json!({"window_id":"1","linux_route":{"terminal_pid":10,
            "client":{"name":"client","pid":11,"tty":"/dev/pts/1","session":"$0"}},
            "tmux_socket":"/socket", "tmux_target":"%0", "tmux_window":"@0"}),
            Platform::Linux,
        )
        .unwrap()
    }

    pub fn tmux_ready(&self) {
        self.response(
            &[
                "-S",
                "/socket",
                "list-clients",
                "-F",
                wrix_notify::navigation::CLIENT_FORMAT,
            ],
            "client|11|/dev/pts/1|$0|@0|%0",
        );
        self.response(
            &[
                "-S",
                "/socket",
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}|#{session_id}",
            ],
            "%0|$0",
        );
        self.response(
            &[
                "-S",
                "/socket",
                "display-message",
                "-p",
                "-t",
                "%0",
                "#{pane_id} #{pane_dead} #{session_id}",
            ],
            "%0 0 $0",
        );
        self.response(
            &[
                "-S",
                "/socket",
                "display-message",
                "-p",
                "-t",
                "%0",
                "#{pane_id} #{pane_dead} #{session_id} #{window_id}",
            ],
            "%0 0 $0 @0",
        );
        for args in [
            vec!["-S", "/socket", "switch-client", "-c", "client", "-t", "$0"],
            vec!["-S", "/socket", "select-window", "-t", "%0"],
            vec!["-S", "/socket", "select-pane", "-t", "%0"],
        ] {
            self.response(&args, "");
        }
    }

    pub fn save(&self, route: &Route) {
        attention::open(&self.directory)
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO return_position VALUES (1, ?)",
                [route.saved().to_string()],
            )
            .unwrap();
    }

    pub fn focused_window(&self) -> Value {
        self.state
            .lock()
            .unwrap()
            .windows
            .as_array()
            .unwrap()
            .iter()
            .find(|window| window["is_focused"] == true)
            .unwrap()["id"]
            .clone()
    }
}

pub fn edit_json(path: &Path, edit: impl FnOnce(&mut Value)) {
    let mut value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    edit(&mut value);
    fs::write(path, value.to_string()).unwrap();
}
