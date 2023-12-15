use matrix_sdk::{config::SyncSettings, Client};

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    let settings = SyncSettings::new().full_state(true);

    //NO_PUSH_main not sure what's going on here
    //let settings = if let Some(token) = client.sync_token().await {
    //    settings.token(token)
    //} else {
    //    settings
    //};
    let _ = client.sync_once(settings).await?;

    let response = client
        .devices()
        .await
        .expect("Can't get devices from server");

    println!("ID\tDevice name\tverified\tcurrent",);

    let current_device = client.device_id().unwrap();

    let user_id = client.user_id().unwrap();
    for device in response.devices {
        let crypt_device = client
            .encryption()
            .get_device(&user_id, &*device.device_id)
            .await
            .unwrap();
        print!(
            "{}\t{}\t{}",
            device.device_id,
            device.display_name.as_deref().unwrap_or(""),
            crypt_device
                .map(|d| d.is_verified().to_string())
                .unwrap_or("unknown".to_owned())
        );
        if device.device_id == current_device {
            println!("\t*");
        } else {
            println!();
        }
    }

    Ok(())
}
