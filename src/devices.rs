use matrix_sdk::Client;

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    let response = client
        .devices()
        .await
        .expect("Can't get devices from server");

    println!("ID\tDevice name\tverified",);

    let user_id = client.user_id().await.unwrap();
    for device in response.devices {
        let crypt_device = client
            .get_device(&user_id, &*device.device_id)
            .await
            .unwrap();
        println!(
            "{}\t{}\t{}",
            device.device_id,
            device.display_name.as_deref().unwrap_or(""),
            crypt_device
                .map(|d| d.verified().to_string())
                .unwrap_or("unknown".to_owned())
        );
    }

    Ok(())
}
