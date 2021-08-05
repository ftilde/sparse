use matrix_sdk::{
    self,
    ruma::events::AnyToDeviceEvent,
    verification::{SasVerification, Verification},
    Client, LoopCtrl, SyncSettings,
};

async fn wait_for_confirmation(sas: SasVerification) {
    println!("Type 'yes' if the emoji or the numbers match:");
    if let Some(emoji) = sas.emoji() {
        print!("Emoji:");
        for (e, description) in emoji {
            print!("{} ({}) ", e, description);
        }
        println!();
    }
    if let Some((n1, n2, n3)) = sas.decimals() {
        println!("Numbers: {}-{}-{}", n1, n2, n3);
    }

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("error: unable to read user input");

    match input.trim().to_lowercase().as_ref() {
        "yes" => {
            sas.confirm().await.unwrap();

            if sas.is_done() {
                print_result(&sas);
            }
        }
        _ => {
            println!("Canceled verification.");
            sas.cancel().await.unwrap();
        }
    }
}

fn print_result(sas: &SasVerification) {
    let device = sas.other_device();

    println!(
        "Successfully verified device {} {} {:?}",
        device.user_id(),
        device.device_id(),
        device.local_trust_state()
    );
}

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    println!(
        "Waiting for verification requests. Initiate verification using another device. This device's id is {}", client.device_id().await.unwrap());
    let client = &client;
    client
        .sync_with_callback(SyncSettings::new(), |response| async move {
            for event in response
                .to_device
                .events
                .iter()
                .filter_map(|e| e.deserialize().ok())
            {
                match event {
                    AnyToDeviceEvent::KeyVerificationStart(e) => {
                        if let Some(Verification::SasV1(sas)) = client
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            println!(
                                "Starting verification with {} {}",
                                &sas.other_device().user_id(),
                                &sas.other_device().device_id()
                            );
                            sas.accept().await.unwrap();
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationKey(e) => {
                        if let Some(Verification::SasV1(sas)) = client
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            tokio::spawn(wait_for_confirmation(sas));
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationMac(e) => {
                        if let Some(Verification::SasV1(sas)) = client
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            if sas.is_done() {
                                print_result(&sas);
                                return LoopCtrl::Break;
                            }
                        }
                    }

                    _ => (),
                }
            }
            LoopCtrl::Continue
        })
        .await;
    Ok(())
}
