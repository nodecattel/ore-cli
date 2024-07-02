use ore::consts::{BUS_ADDRESSES, TOKEN_DECIMALS};
use ore::state::Bus;
// use ore::utils::AccountDeserialize;
use crate::Miner;
use utils::AccountDeserialize;

impl Miner {
    pub async fn busses(&self) {
        let client = self.rpc_client.clone();
        for address in BUS_ADDRESSES.iter() {
            let data = client.get_account_data(address).await.unwrap();
            match Bus::try_from_bytes(&data) {
                Ok(bus) => {
                    let rewards = (bus.rewards as f64) / 10f64.powf(TOKEN_DECIMALS as f64);
                    println!("Bus {}: {:} ORE", bus.id, rewards);
                }
                Err(_) => {}
            }
        }
    }
}
