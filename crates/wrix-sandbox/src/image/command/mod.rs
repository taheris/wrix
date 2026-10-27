//! Thread-scoped immutable native image command recipes, never process-wide PATH mutation.
use super::Error;
use std::{cell::RefCell, ffi::OsString, path::PathBuf, process::Command};

#[derive(Clone)]
struct Recipe {
    program: PathBuf,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    directory: PathBuf,
}
impl Recipe {
    fn parse(command: &Command) -> Result<Self, Error> {
        let program = PathBuf::from(command.get_program());
        if !program.is_absolute() {
            return Err(Error::InvalidMetadata);
        }
        let directory = command
            .get_current_dir()
            .map_or_else(|| PathBuf::from("/"), std::path::Path::to_path_buf);
        if !directory.is_absolute() {
            return Err(Error::InvalidMetadata);
        }
        Ok(Self {
            program,
            arguments: command.get_args().map(std::ffi::OsStr::to_owned).collect(),
            environment: command
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value.to_owned())))
                .collect(),
            directory,
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .env_clear()
            .envs(self.environment.iter().cloned())
            .current_dir(&self.directory);
        command
    }
}
#[derive(Clone)]
struct Recipes {
    container: Recipe,
    skopeo: Recipe,
}
thread_local! { static ACTIVE: RefCell<Option<Recipes>> = const { RefCell::new(None) }; }
struct Restore(Option<Recipes>);
impl Drop for Restore {
    fn drop(&mut self) {
        drop(ACTIVE.with(|active| active.replace(self.0.take())));
    }
}

/// Native commands inherit only explicitly configured template variables; nesting and unwinding restore the prior scope.
pub(super) fn scope<T, E: From<Error>>(
    container: &Command,
    skopeo: &Command,
    callback: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let recipes = Recipes {
        container: Recipe::parse(container)?,
        skopeo: Recipe::parse(skopeo)?,
    };
    let _restore = Restore(ACTIVE.with(|active| active.replace(Some(recipes))));
    callback()
}

pub(super) fn selected(program: &str) -> Option<Command> {
    ACTIVE.with(|active| {
        active.borrow().as_ref().and_then(|recipes| match program {
            "container" => Some(recipes.container.command()),
            "skopeo" => Some(recipes.skopeo.command()),
            _ => None,
        })
    })
}

#[cfg(test)]
mod test;
