//! YouGile [`LivenessProbe`] stub per ADR-021 §6.

use async_trait::async_trait;
use devboy_core::LivenessProbe;

use crate::client::YouGileClient;

#[async_trait]
impl LivenessProbe for YouGileClient {
    fn provider_name(&self) -> &str {
        "yougile"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::liveness::LivenessStatus;
    use secrecy::SecretString;

    #[tokio::test]
    async fn provider_name_is_yougile_and_default_returns_not_implemented() {
        let client = YouGileClient::new("board-1", SecretString::from("any".to_owned()));
        assert_eq!(client.provider_name(), "yougile");
        let result = client
            .test(&SecretString::from("any".to_owned()))
            .await
            .unwrap();
        assert_eq!(result.status, LivenessStatus::NotImplemented);
    }
}
