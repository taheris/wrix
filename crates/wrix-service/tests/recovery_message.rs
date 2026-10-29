use wrix_service::lifecycle::{Error, RuntimeStatus};

#[test]
fn unavailable_owner_suggests_only_short_recovery_command() {
    for status in [RuntimeStatus::Missing, RuntimeStatus::Stopped] {
        let error = Error::CellerUnavailable {
            container: "wrix-host-service".into(),
            status,
        };
        assert_eq!(
            error.to_string(),
            format!(
                "selected Celler owner wrix-host-service is {status:?}; run `wrix service recover`"
            )
        );
    }
}
