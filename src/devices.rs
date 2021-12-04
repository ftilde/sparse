use matrix_sdk::{config::SyncSettings, Client};

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    let _ = client
        .sync_once(
            SyncSettings::new()
                .full_state(true)
                .token(client.sync_token().await.unwrap()),
        )
        .await?;

    let response = client
        .devices()
        .await
        .expect("Can't get devices from server");

    println!("ID\tDevice name\tverified\tcurrent",);

    let current_device = client.device_id().await.unwrap();

    let user_id = client.user_id().await.unwrap();
    for device in response.devices {
        let crypt_device = client
            .get_device(&user_id, &*device.device_id)
            .await
            .unwrap();
        print!(
            "{}\t{}\t{}",
            device.device_id,
            device.display_name.as_deref().unwrap_or(""),
            crypt_device
                .map(|d| d.verified().to_string())
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
