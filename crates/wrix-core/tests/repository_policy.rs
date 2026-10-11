use std::{fmt::Write, fs, io};

use wrix_core::{
    deploy_key,
    git::remote,
    repository_policy::{self, ParseError, Policy, ReadError},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn omitted_policy_fields_remain_absent_and_grants_default_false() {
    for content in ["", "# no overrides\n", "[cache]\n", "[git]\n", "[init]\n"] {
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
            let mut content = String::from("[git]\n");
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
    let policy = repository_policy::parse("git.deploy_key = 'repo-key'\n").unwrap();
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
        cache: repository_policy::cache::Policy::default(),
    };
    for content in [
        "[git]\ndeploy_key = 'repo.example-key_1'\ndeploy = true\nsign = false\nremote = 'upstream'\n[init]\nprek_hooks = false\nonline_verify = true\n",
        "git = { deploy_key = 'repo.example-key_1', deploy = true, sign = false, remote = 'upstream' }\ninit = { prek_hooks = false, online_verify = true }\n",
    ] {
        assert_eq!(repository_policy::parse(content).unwrap(), expected);
    }
}

#[test]
fn init_boolean_overrides_preserve_omission_and_explicit_values() {
    for key in ["prek_hooks", "online_verify"] {
        for value in [false, true] {
            let policy = repository_policy::parse(&format!("init.{key} = {value}\n")).unwrap();
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
            let content = format!("git.{key} = {value}\n");
            let error = repository_policy::parse(&content).unwrap_err();
            assert!(matches!(error, ParseError::Toml(_)), "accepted {content:?}");
        }
    }
}

#[test]
fn retired_sign_commits_is_rejected_even_with_valid_grants() {
    for value in ["true", "false", "'false'", "0", "{}"] {
        for grants in ["", "deploy = true\nsign = true\n"] {
            let content = format!("[git]\n{grants}sign_commits = {value}\n");
            let error = repository_policy::parse(&content).unwrap_err();
            assert!(matches!(error, ParseError::Toml(_)));
            assert!(error.to_string().contains("unknown field `sign_commits`"));
        }
    }
}

#[test]
fn malformed_toml_duplicate_fields_and_wrong_table_types_are_rejected() {
    for content in [
        "[git",
        "wrix = false",
        "wrix = []",
        "git = 'invalid'",
        "git = []",
        "git = ['repo-key', true, false, 'origin']",
        "[[git]]\ndeploy = true",
        "init = true",
        "init = []",
        "init = [true, false]",
        "[[init]]\nprek_hooks = false",
        "git.deploy = true\ngit.deploy = false",
        "git.sign = true\ngit.sign = false",
        "git.deploy_key = 'one'\ngit.deploy_key = 'two'",
        "git.remote = 'one'\ngit.remote = 'two'",
        "init.prek_hooks = true\ninit.prek_hooks = false",
        "init.online_verify = true\ninit.online_verify = false",
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
            let content = format!("{key} = {value}\n");
            assert!(matches!(
                repository_policy::parse(&content),
                Err(ParseError::Toml(_))
            ));
        }
    }
    for key in ["init.prek_hooks", "init.online_verify"] {
        for value in ["'false'", "42", "[]", "{}"] {
            let content = format!("{key} = {value}\n");
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
        let content = format!("git.deploy_key = '{name}'\n");
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
        let content = format!("git.remote = '{name}'\n");
        let policy = repository_policy::parse(&content).unwrap();
        let remote = policy.git.remote.unwrap();
        assert_eq!(remote.as_str(), name.trim());
        assert_eq!(remote.to_string(), name.trim());
    }
    for name in ["", " ", "two words", "two\twords"] {
        let content = format!("git.remote = '{name}'\n");
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
fn unknown_policy_sections_and_keys_are_rejected() {
    for content in [
        "label = 'extension'",
        "[extension]\nvalue = [1, 2]",
        "git.sign = true\n[extension]\nvalue = [1, 2]",
        "git.extension = true",
        "init.extension = true",
        "cache.extension = true",
        "cache.warm.extension = true",
        "git.signing = true",
        "git.deployKey = 'repo-key'",
    ] {
        assert!(
            matches!(repository_policy::parse(content), Err(ParseError::Toml(_))),
            "accepted {content:?}"
        );
    }
}

#[test]
fn prefixed_policy_is_rejected_instead_of_ignored() {
    for content in [
        "[wrix]",
        "[wrix.git]\ndeploy = true",
        "[wrix.init]\nonline_verify = false",
        "[wrix.cache]\nwarn_size = '1G'",
        "wrix.git = { sign = true }",
        "wrix.git.sign = true",
        "git.sign = true\nwrix.git.sign = false",
        "[git]\nsign = true\n[wrix.git]\nsign = false",
    ] {
        let error = repository_policy::parse(content).unwrap_err();
        assert!(
            error.to_string().contains("unknown field `wrix`"),
            "{content:?}: {error}"
        );
    }
}

#[test]
fn cache_quantities_accept_documented_units_and_plain_numbers() {
    for (key, value, expected) in [
        ("warn_size", "'1K'", 1024),
        ("warn_size", "'1M'", 1_048_576),
        ("warn_size", "'1G'", 1_073_741_824),
        ("warn_size", "'1T'", 1_099_511_627_776),
        ("warn_size", "0", 0),
        ("warn_size", "'12'", 12),
        ("pending_ttl", "'7d'", 604_800),
        ("pending_ttl", "12", 12),
        ("prune_interval", "'24h'", 86_400),
        ("lock_timeout", "'2m'", 120),
        ("explicit_lock_timeout", "'1s'", 1),
    ] {
        let policy = repository_policy::parse(&format!("cache.{key} = {value}")).unwrap();
        let actual = match key {
            "warn_size" => policy.cache.warn_size.unwrap().get(),
            "pending_ttl" => policy.cache.pending_ttl.unwrap().get(),
            "prune_interval" => policy.cache.prune_interval.unwrap().get(),
            "lock_timeout" => policy.cache.lock_timeout.unwrap().get(),
            "explicit_lock_timeout" => policy.cache.explicit_lock_timeout.unwrap().get(),
            _ => panic!("unknown test field"),
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn invalid_cache_quantities_and_wrong_typed_roots_are_rejected() {
    for content in [
        "cache.warn_size = -1",
        "cache.warn_size = '1GiB'",
        "cache.warn_size = '18446744073709551615G'",
        "cache.pending_ttl = ''",
        "cache.prune_interval = '1G'",
        "cache.lock_timeout = '1.5s'",
        "cache.warn_size = true",
        "cache.warm = []",
        "cache.publish.packages = 'false'",
        "cache.warm.include_roots = 'a'",
        "cache.warm.exclude_roots = [1]",
        "cache = []",
    ] {
        assert!(
            repository_policy::parse(content).is_err(),
            "accepted {content:?}"
        );
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
    let content = "git = { deploy_key = 'repo-key', deploy = false, sign = true, remote = 'upstream' }\ninit = { prek_hooks = false, online_verify = false }\n";
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
        "git.deploy = 'true'",
        "git.sign = 42",
        "git.sign_commits = false",
        "git.deploy_key = '../outside'",
        "git.remote = 'two words'",
        "init.online_verify = 'false'",
        "[git",
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
    fs::write(&path, "git.sign = true\n")?;
    assert!(repository_policy::read(root.path())?.git.sign_enabled());
    fs::write(&path, "git.deploy = true\n")?;
    let policy = repository_policy::read(root.path())?;
    assert!(policy.git.deploy_enabled());
    assert!(!policy.git.sign_enabled());
    fs::remove_file(&path)?;
    assert_eq!(repository_policy::read(root.path())?, Policy::default());
    Ok(())
}
