use matrix_sdk::ruma::events::key::verification::VerificationMethod;

use matrix_sdk::{self, config::SyncSettings, Client};

pub async fn run(client: Client, id: String) -> Result<(), matrix_sdk::Error> {
    let _res = client.sync_once(SyncSettings::new()).await?;
    let user_id = client.user_id().unwrap();
    let device = client
        .encryption()
        .get_device(&user_id, id.as_str().into())
        .await?
        .unwrap();

    let _ = device
        .request_verification_with_methods(vec![VerificationMethod::SasV1])
        .await?;

    crate::verification_common::run_verify_loop(&client).await
}
