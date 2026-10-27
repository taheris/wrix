use super::*;

fn executable(name: &str) -> PathBuf {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap()) {
        let path = directory.join(name);
        if path.is_absolute() && path.is_file() {
            return path;
        }
    }
    panic!("native test prerequisite {name} is missing");
}
fn recipe(value: &str) -> Command {
    let mut command = Command::new(executable("printf"));
    command
        .env_clear()
        .env("WRIX_IMAGE_SCOPE_TEST", value)
        .current_dir("/")
        .args(["%s:%s", value]);
    command
}
fn output() -> Result<String, Error> {
    let output = crate::image::run_output("container", &["inspect"])?;
    assert!(output.status.success());
    Ok(String::from_utf8(output.stdout).unwrap())
}

#[test]
fn scoped_commands_execute_exact_recipes_and_restore_nested_selection() {
    let outer = recipe("outer");
    let inner = recipe("inner");
    assert!(selected("container").is_none());
    scope(&outer, &outer, || -> Result<(), Error> {
        assert_eq!(output()?, "outer:inspect");
        let command = selected("container").unwrap();
        assert_eq!(command.get_current_dir(), Some(std::path::Path::new("/")));
        scope(&inner, &inner, || -> Result<(), Error> {
            assert_eq!(output()?, "inner:inspect");
            Ok(())
        })?;
        assert_eq!(output()?, "outer:inspect");
        Ok(())
    })
    .unwrap();
    assert!(selected("container").is_none());
}

#[test]
fn native_command_inherits_only_explicit_template_environment() {
    let mut command = Command::new(executable("env"));
    command.env_clear().env("WRIX_IMAGE_SCOPE_TEST", "explicit");
    scope(&command, &command, || -> Result<(), Error> {
        let output = crate::image::run_output("container", &[])?;
        assert!(output.status.success());
        assert_eq!(output.stdout, b"WRIX_IMAGE_SCOPE_TEST=explicit\n");
        Ok(())
    })
    .unwrap();
}

#[test]
fn image_scope_does_not_leak_to_other_threads_or_process_environment() {
    let command = recipe("only-this-thread");
    let original = std::env::var_os("WRIX_IMAGE_SCOPE_TEST");
    scope(&command, &command, || -> Result<(), Error> {
        assert!(
            std::thread::spawn(|| selected("container").is_none())
                .join()
                .unwrap()
        );
        assert_eq!(std::env::var_os("WRIX_IMAGE_SCOPE_TEST"), original);
        Ok(())
    })
    .unwrap();
    assert_eq!(std::env::var_os("WRIX_IMAGE_SCOPE_TEST"), original);
}

#[test]
fn unwinding_restores_the_previous_image_command_scope() {
    let command = recipe("outer");
    scope(&command, &command, || -> Result<(), Error> {
        let result = std::panic::catch_unwind(|| {
            let inner = recipe("inner");
            scope(&inner, &inner, || -> Result<(), Error> {
                panic!("test-only unwind");
            })
        });
        assert!(result.is_err());
        assert_eq!(output()?, "outer:inspect");
        Ok(())
    })
    .unwrap();
    assert!(selected("container").is_none());
}

#[test]
fn an_ambient_alias_cannot_enter_a_package_scoped_recipe() {
    let alias = Command::new("container");
    assert!(matches!(
        scope(&alias, &recipe("fixed"), || -> Result<(), Error> { Ok(()) }),
        Err(Error::InvalidMetadata)
    ));
    assert!(selected("container").is_none());
}
