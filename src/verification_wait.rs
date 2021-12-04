use matrix_sdk::Client;

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    println!(
        "Waiting for verification requests. Initiate verification using another device. This device's id is {}", client.device_id().await.unwrap());
    let client = &client;
    loop {
        crate::verification_common::run_verify_loop(client).await?;
    }
}
