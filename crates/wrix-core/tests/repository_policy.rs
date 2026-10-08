use std::{fmt::Write, fs, io};

use wrix_core::{
    deploy_key,
    git::remote,
    repository_policy::{self, ParseError, Policy, ReadError},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn omitted_policy_fields_remain_absent_and_grants_default_false() {
    for content in [
        "",
        "# no overrides\n",
        "[wrix]\n",
        "[wrix.git]\n",
        "[wrix.init]\n",
    ] {
        let policy = repository_policy::parse(content).unwrap();
        assert_eq!(policy, Policy::default(), "parsed {content:?}");
        assert!(!policy.git.deploy_enabled());
        assert!(!policy.git.sign_enabled());
    }
}

#[test]
fn optional_git_grants_are_independent_for_every_combination() {
    for deploy in [None, Some(false), Some(true)] {
        for sign in [None, Some(false), Some(true)] {
            let mut content = String::from("[wrix.git]\n");
            if let Some(value) = deploy {
                writeln!(content, "deploy = {value}").unwrap();
            }
            if let Some(value) = sign {
                writeln!(content, "sign = {value}").unwrap();
            }
            let policy = repository_policy::parse(&content).unwrap();
            assert_eq!(policy.git.deploy, deploy);
            assert_eq!(policy.git.sign, sign);
            assert_eq!(policy.git.deploy_enabled(), deploy == Some(true));
            assert_eq!(policy.git.sign_enabled(), sign == Some(true));
        }
    }
}

#[test]
fn key_identity_alone_does_not_grant_credentials() {
    let policy = repository_policy::parse("wrix.git.deploy_key = 'repo-key'\n").unwrap();
    assert_eq!(
        policy.git.deploy_key,
        Some(deploy_key::Name::parse("repo-key").unwrap())
    );
    assert_eq!(policy.git.deploy, None);
    assert_eq!(policy.git.sign, None);
    assert!(!policy.git.deploy_enabled());
    assert!(!policy.git.sign_enabled());
}

#[test]
fn supported_key_remote_and_init_overrides_are_typed() {
    let expected = Policy {
        git: repository_policy::Git {
            deploy_key: Some(deploy_key::Name::parse("repo.example-key_1").unwrap()),
            deploy: Some(true),
            sign: Some(false),
            remote: Some(remote::Name::parse("upstream").unwrap()),
        },
        init: repository_policy::Init {
            prek_hooks: Some(false),
            online_verify: Some(true),
        },
    };
    for content in [
        "[wrix.git]\ndeploy_key = 'repo.example-key_1'\ndeploy = true\nsign = false\nremote = 'upstream'\n[wrix.init]\nprek_hooks = false\nonline_verify = true\n",
        "wrix.git = { deploy_key = 'repo.example-key_1', deploy = true, sign = false, remote = 'upstream' }\nwrix.init = { prek_hooks = false, online_verify = true }\n",
    ] {
        assert_eq!(repository_policy::parse(content).unwrap(), expected);
    }
}

#[test]
fn init_boolean_overrides_preserve_omission_and_explicit_values() {
    for key in ["prek_hooks", "online_verify"] {
        for value in [false, true] {
            let policy = repository_policy::parse(&format!("wrix.init.{key} = {value}\n")).unwrap();
            if key == "prek_hooks" {
                assert_eq!(policy.init.prek_hooks, Some(value));
                assert_eq!(policy.init.online_verify, None);
            } else {
                assert_eq!(policy.init.prek_hooks, None);
                assert_eq!(policy.init.online_verify, Some(value));
            }
        }
    }
}

#[test]
fn non_boolean_grants_are_rejected() {
    for key in ["deploy", "sign"] {
        for value in [
            "'true'",
            "'false'",
            "0",
            "1",
            "1.0",
            "[]",
            "{}",
            "1979-05-27",
        ] {
            let content = format!("wrix.git.{key} = {value}\n");
            let error = repository_policy::parse(&content).unwrap_err();
            assert!(matches!(error, ParseError::Toml(_)), "accepted {content:?}");
        }
    }
}

#[test]
fn retired_sign_commits_is_rejected_even_with_valid_grants() {
    for value in ["true", "false", "'false'", "0", "{}"] {
        for grants in ["", "deploy = true\nsign = true\n"] {
            let content = format!("[wrix.git]\n{grants}sign_commits = {value}\n");
            let error = repository_policy::parse(&content).unwrap_err();
            assert!(matches!(error, ParseError::Toml(_)));
            assert!(error.to_string().contains("unknown field `sign_commits`"));
        }
    }
}

#[test]
fn malformed_toml_duplicate_fields_and_wrong_table_types_are_rejected() {
    for content in [
        "[wrix.git",
        "wrix = false",
        "wrix = []",
        "wrix.git = 'invalid'",
        "wrix.git = []",
        "wrix.git = ['repo-key', true, false, 'origin']",
        "[[wrix.git]]\ndeploy = true",
        "wrix.init = true",
        "wrix.init = []",
        "wrix.init = [true, false]",
        "[[wrix.init]]\nprek_hooks = false",
        "wrix.git.deploy = true\nwrix.git.deploy = false",
        "wrix.git.sign = true\nwrix.git.sign = false",
        "wrix.git.deploy_key = 'one'\nwrix.git.deploy_key = 'two'",
        "wrix.git.remote = 'one'\nwrix.git.remote = 'two'",
        "wrix.init.prek_hooks = true\nwrix.init.prek_hooks = false",
        "wrix.init.online_verify = true\nwrix.init.online_verify = false",
    ] {
        assert!(
            matches!(repository_policy::parse(content), Err(ParseError::Toml(_))),
            "accepted {content:?}"
        );
    }
}

#[test]
fn wrong_typed_key_remote_and_init_values_are_rejected() {
    for key in ["git.deploy_key", "git.remote"] {
        for value in ["true", "42", "[]", "{}"] {
            let content = format!("wrix.{key} = {value}\n");
            assert!(matches!(
                repository_policy::parse(&content),
                Err(ParseError::Toml(_))
            ));
        }
    }
    for key in ["init.prek_hooks", "init.online_verify"] {
        for value in ["'false'", "42", "[]", "{}"] {
            let content = format!("wrix.{key} = {value}\n");
            assert!(matches!(
                repository_policy::parse(&content),
                Err(ParseError::Toml(_))
            ));
        }
    }
}

#[test]
fn unsafe_deploy_key_names_are_rejected() {
    for name in [
        "",
        " ",
        ".",
        "..",
        "/tmp/key",
        "nested/key",
        "nested\\key",
        "two words",
        " repo-key",
        "repo-key ",
    ] {
        let content = format!("wrix.git.deploy_key = '{name}'\n");
        assert!(
            matches!(
                repository_policy::parse(&content),
                Err(ParseError::DeployKey(_))
            ),
            "accepted {name:?}"
        );
    }
}

#[test]
fn remote_names_preserve_existing_normalization_and_constraints() {
    for name in ["origin", " upstream ", "team/upstream", "-custom"] {
        let content = format!("wrix.git.remote = '{name}'\n");
        let policy = repository_policy::parse(&content).unwrap();
        let remote = policy.git.remote.unwrap();
        assert_eq!(remote.as_str(), name.trim());
        assert_eq!(remote.to_string(), name.trim());
    }
    for name in ["", " ", "two words", "two\twords"] {
        let content = format!("wrix.git.remote = '{name}'\n");
        assert!(
            matches!(
                repository_policy::parse(&content),
                Err(ParseError::Remote(_))
            ),
            "accepted {name:?}"
        );
    }
}

#[test]
fn unrelated_root_extensions_remain_accepted_without_becoming_policy() {
    for content in [
        "label = 'extension'\n",
        "[extension.wrix.git]\ndeploy = true\nsign_commits = true\n",
        "[git]\ndeploy = true\nsign = true\n",
        "[init]\nprek_hooks = false\nonline_verify = false\n",
    ] {
        assert_eq!(
            repository_policy::parse(content).unwrap(),
            Policy::default()
        );
    }
    let policy =
        repository_policy::parse("wrix.git.sign = true\n[extension]\nvalue = [1, 2]\n").unwrap();
    assert_eq!(policy.git.sign, Some(true));
    assert!(!policy.git.deploy_enabled());
}

#[test]
fn unknown_wrix_fields_remain_rejected() {
    for content in [
        "wrix.extension = true",
        "wrix.git.extension = true",
        "wrix.init.extension = true",
        "wrix.git.signing = true",
        "wrix.git.deployKey = 'repo-key'",
    ] {
        assert!(matches!(
            repository_policy::parse(content),
            Err(ParseError::Toml(_))
        ));
    }
}

#[test]
fn missing_policy_is_default_and_does_not_create_files() -> TestResult {
    let root = tempfile::tempdir()?;
    assert_eq!(repository_policy::read(root.path())?, Policy::default());
    assert_eq!(fs::read_dir(root.path())?.count(), 0);
    Ok(())
}

#[test]
fn reading_existing_policy_does_not_mutate_repository_files() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("wrix.toml");
    let content = "wrix.git = { deploy_key = 'repo-key', deploy = false, sign = true, remote = 'upstream' }\nwrix.init = { prek_hooks = false, online_verify = false }\n";
    fs::write(&path, content)?;
    fs::create_dir(root.path().join(".git"))?;
    let git_config = root.path().join(".git/config");
    fs::write(&git_config, "unchanged Git config\n")?;
    let modified = fs::metadata(&path)?.modified()?;

    assert_eq!(
        repository_policy::read(root.path())?,
        repository_policy::parse(content)?
    );
    assert_eq!(fs::read_to_string(&path)?, content);
    assert_eq!(fs::metadata(&path)?.modified()?, modified);
    assert_eq!(fs::read_to_string(git_config)?, "unchanged Git config\n");
    assert_eq!(fs::read_dir(root.path())?.count(), 2);
    Ok(())
}

#[test]
fn invalid_policy_errors_include_path_without_mutating_repository_files() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("wrix.toml");
    fs::create_dir(root.path().join(".git"))?;
    let git_config = root.path().join(".git/config");
    fs::write(&git_config, "unchanged Git config\n")?;
    for content in [
        "wrix.git.deploy = 'true'",
        "wrix.git.sign = 42",
        "wrix.git.sign_commits = false",
        "wrix.git.deploy_key = '../outside'",
        "wrix.git.remote = 'two words'",
        "wrix.init.online_verify = 'false'",
        "[wrix.git",
    ] {
        fs::write(&path, content)?;
        let error = repository_policy::read(root.path()).unwrap_err();
        assert!(
            matches!(&error, ReadError::Policy { path: error_path, .. } if error_path == &path)
        );
        assert!(error.to_string().contains(&path.display().to_string()));
        assert_eq!(fs::read_to_string(&path)?, content);
        assert_eq!(fs::read_to_string(&git_config)?, "unchanged Git config\n");
        assert_eq!(fs::read_dir(root.path())?.count(), 2);
    }
    Ok(())
}

#[test]
fn policy_read_failures_are_not_treated_as_absent_policy() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("wrix.toml");
    fs::create_dir(&path)?;
    assert!(
        matches!(repository_policy::read(root.path()), Err(ReadError::Io { path: error_path, .. }) if error_path == path)
    );
    fs::remove_dir(&path)?;
    fs::write(&path, [0xff])?;
    assert!(
        matches!(repository_policy::read(root.path()), Err(ReadError::Io { path: error_path, source }) if error_path == path && source.kind() == io::ErrorKind::InvalidData)
    );
    Ok(())
}

#[test]
fn repository_policy_is_read_fresh_on_each_call() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("wrix.toml");
    fs::write(&path, "wrix.git.sign = true\n")?;
    assert!(repository_policy::read(root.path())?.git.sign_enabled());
    fs::write(&path, "wrix.git.deploy = true\n")?;
    let policy = repository_policy::read(root.path())?;
    assert!(policy.git.deploy_enabled());
    assert!(!policy.git.sign_enabled());
    fs::remove_file(&path)?;
    assert_eq!(repository_policy::read(root.path())?, Policy::default());
    Ok(())
}
