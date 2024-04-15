use std::{
    // fmt::Debug, 
	io::{stdout, Write}, 
	sync::{atomic::AtomicBool, Arc, Mutex}, 
	time::Duration
};
use chrono;
use std::time::Instant;
use ore::{self, state::Bus, BUS_ADDRESSES, BUS_COUNT, EPOCH_DURATION};
use rand::Rng;
use solana_program::{keccak::HASH_BYTES, program_memory::sol_memcmp, pubkey::Pubkey};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    keccak::{hashv, Hash as KeccakHash},
    signature::Signer,
};

use crate::{
    cu_limits::{CU_LIMIT_MINE, CU_LIMIT_RESET},
    utils::{get_clock_account, get_proof, get_treasury},
    Miner,
};

// Odds of being selected to submit a reset tx
const RESET_ODDS: u64 = 20;

impl Miner {
    pub async fn mine(&self, threads: u64) {
        // Register, if needed.
        let signer = self.signer();
        self.register().await;
        // let mut stdout = stdout();
        // let stdout = stdout();
        let mut rng = rand::thread_rng();
        let mut session_mined = 0;
        let mut mining_passes = 0;
		let mut initial_rewards = 0.0;
		let mut initial_sol_balance = 0.0;
		let mut automated_priority_fee: u64 = self.priority_fee;

        // Start mining loop
        loop {
			mining_passes+=1;
			let pass_start_time = Instant::now();
		
		    // Fetch account state
            let balance = self.get_ore_display_balance().await;
			let sol_balance = self.get_sol_display_balance().await;
			let treasury = get_treasury(&self.rpc_client).await;
            let proof = get_proof(&self.rpc_client, signer.pubkey()).await;
            let rewards = (proof.claimable_rewards as f64) / (10f64.powf(ore::TOKEN_DECIMALS as f64));
            let reward_rate = (treasury.reward_rate as f64) / (10f64.powf(ore::TOKEN_DECIMALS as f64));

			// Set the initial rewards amount to calc what has been added in this mining session
			if mining_passes == 1 {
				initial_rewards=rewards;
				initial_sol_balance=sol_balance;
			} else {			
            	println!("\n\n\n\n\n");								// Add a few empty lines between passes
				println!("-------------------------------------------------------------------------------");
			}
            // stdout.write_all(b"\x1b[2J\x1b[3J\x1b[H").ok();	// Clear the terminal windows - hides previous path from scroll buffer
            println!("Mining Pass {} started at {}", mining_passes,  chrono::offset::Local::now());
			println!("-------------------------------------------------------------------------------");
			if sol_balance<0.000005 {
				println!("Mining is suspended for 1 minute - no SOL available in wallet.");
				// Delay to prevent overloading your RPC & give the transaction a chance to process
				std::thread::sleep(Duration::from_millis(60000));

			} else { // Attempt to mine
				// stdout.write_all(b"\x1b[2J\x1b[3J\x1b[H").ok();
				println!("Wallet:\t\t\t{}", &signer.pubkey());
				println!("RPC_URL:\t\t{}", self.rpc_url);
				println!("Initial SOL Price:\t${:.2}", self.initial_sol_price);
				println!("Initial ORE Price:\t${:.2}", self.initial_ore_price);
				println!("SOL Balance:\t\t{:.9}\t${:.2}\tUsed: {:.9}\t${:.2}", 
						sol_balance, sol_balance*self.initial_sol_price, 
						(initial_sol_balance-sol_balance), (initial_sol_balance-sol_balance)*self.initial_sol_price);
				println!("ORE Balance:\t\t{:.9}\t${:.2}", balance, balance*self.initial_ore_price);
				println!("ORE Claimable:\t\t{:.9}\t${:.2}", rewards, rewards*self.initial_ore_price);
				println!("Session Rewards:\t{:.9}\t${:.2}\t(succeeded {} times)", 
						(rewards-initial_rewards), (rewards-initial_rewards)*self.initial_ore_price, session_mined, );
				println!("Reward Rate:\t\t{:.9}\t${:.3}", reward_rate, reward_rate*self.initial_ore_price);

				// Escape sequence that clears the screen and the scrollback buffer
				let hash_start_time= Instant::now();
				println!("\nMining for a valid hash...");
				let (next_hash, nonce) =
					self.find_next_hash_par(proof.hash.into(), treasury.difficulty.into(), threads);
				let hash_duration = hash_start_time.elapsed();

				// Submit mine tx.
				// Use busses randomly so on each epoch, transactions don't pile on the same busses
				let submit_start_time= Instant::now();
				println!("\n\nSubmitting hash for validation...");
				let mut attempts = 0;
				'submit: loop {
                attempts += 1;
                println!("-------------------------------------------------------------------------------");
				let last_submit_start_time= Instant::now();

				// Double check we're submitting for the right challenge
                let proof_ = get_proof(&self.rpc_client, signer.pubkey()).await;
                if !self.validate_hash(
                    next_hash,
                    proof_.hash.into(),
                    signer.pubkey(),
                    nonce,
                    treasury.difficulty.into(),
                ) {
                    println!("Submit Hash {}:\tHash already validated! An earlier transaction must have landed.", attempts);
                    break 'submit;
                }

                // Reset epoch, if needed
                let treasury = get_treasury(&self.rpc_client).await;
                let clock = get_clock_account(&self.rpc_client).await;
                let threshold = treasury.last_reset_at.saturating_add(EPOCH_DURATION);
                if clock.unix_timestamp.ge(&threshold) {
                    // There are a lot of miners right now, so randomly select into submitting tx
                    if rng.gen_range(0..RESET_ODDS).eq(&0) {
                        println!("Submit Hash {}:\tSending epoch reset transaction...", attempts);
                        let cu_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(CU_LIMIT_RESET);
                        let cu_price_ix = ComputeBudgetInstruction::set_compute_unit_price(automated_priority_fee);
                        let reset_ix = ore::instruction::reset(signer.pubkey());
                        self.send_and_confirm(&[cu_limit_ix, cu_price_ix, reset_ix], false, true, automated_priority_fee)
                            .await
                            .ok();
                    }
                }

                // Submit request.
                let bus = self.find_bus_id(treasury.reward_rate).await;
                let bus_rewards = (bus.rewards as f64) / (10f64.powf(ore::TOKEN_DECIMALS as f64));
				print!("Submit Hash {}:\tSending on bus {} ({} ORE) priority_fee: {:?}\t", attempts, bus.id, bus_rewards, automated_priority_fee);
                let cu_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(CU_LIMIT_MINE);
                let cu_price_ix = ComputeBudgetInstruction::set_compute_unit_price(automated_priority_fee);
                let ix_mine = ore::instruction::mine(
                    signer.pubkey(),
                    BUS_ADDRESSES[bus.id as usize],
                    next_hash.into(),
                    nonce,
                );
                match self
                    .send_and_confirm(&[cu_limit_ix, cu_price_ix, ix_mine], false, false, automated_priority_fee)
                    .await
                {
                    Ok(sig) => {
                        println!("\nSubmit Hash {}:\t[SUCCESS] Rewards available in wallet. TX ID: {}", attempts, sig);
						session_mined+=1;
						let submit_duration = submit_start_time.elapsed();
						let last_submit_duration= last_submit_start_time.elapsed();
						let pass_duration = pass_start_time.elapsed();
                        println!("Submit Hash {}:\t[SUCCESS] Time for pass execution: (hash){}s + (submit){}s = {}s for entire pass", 
								attempts, hash_duration.as_secs(), submit_duration.as_secs(), pass_duration.as_secs());

						// Reduce the priority_fee if transaction suceeded quickly
						if last_submit_duration.as_secs()<30 && automated_priority_fee>5000 {
							automated_priority_fee-=5000;
							println!("\t\t[SUCCESS] Reducing priority_fee to {} as submit took less than 30s.", automated_priority_fee)
						} else if last_submit_duration.as_secs()>60 && automated_priority_fee<250000 && attempts==1 {
							automated_priority_fee+=5000;
							println!("\t\t[SUCCESS] Increasing priority_fee to {} as submit took more than 60s.", automated_priority_fee)
						} else {
							println!("\t\t[SUCCESS] No adjustment made to next priority_fee {}.", automated_priority_fee)
						}
						break;
                    }
                    Err(_err) => {
                        // TODO
						eprintln!("\nSubmit Hash {}:\t[ERROR]\tFailed to submit hash. Will retry on another bus...", attempts);
						// Increment the priority_fee if we get completely fail on a bus.
						// if automated_priority_fee<250000 {
						// 	automated_priority_fee+=5000;
						// 	println!("\t\t[ERROR] Increasing priority_fee to {} as failed to submit transaction", automated_priority_fee)
						// } else {
						// 	println!("\t\t[ERROR] No adjustment made to next priority_fee (AT MAX) {}.", automated_priority_fee)
						// }
                    }
                }
            }
			}
		}
    }

    async fn find_bus_id(&self, reward_rate: u64) -> Bus {
        let mut rng = rand::thread_rng();
        loop {
            let bus_id = rng.gen_range(0..BUS_COUNT);
            if let Ok(bus) = self.get_bus(bus_id).await {
                if bus.rewards.gt(&reward_rate.saturating_mul(20)) {
                    return bus;
                }
            }
        }
    }

    fn _find_next_hash(&self, hash: KeccakHash, difficulty: KeccakHash) -> (KeccakHash, u64) {
        let signer = self.signer();
        let mut next_hash: KeccakHash;
        let mut nonce = 0u64;
        loop {
            next_hash = hashv(&[
                hash.to_bytes().as_slice(),
                signer.pubkey().to_bytes().as_slice(),
                nonce.to_le_bytes().as_slice(),
            ]);
            if next_hash.le(&difficulty) {
                break;
            } else {
                println!("Invalid hash: {} Nonce: {:?}", next_hash.to_string(), nonce);
            }
            nonce += 1;
        }
        (next_hash, nonce)
    }

    fn find_next_hash_par(
        &self,
        hash: KeccakHash,
        difficulty: KeccakHash,
        threads: u64,
    ) -> (KeccakHash, u64) {
        let found_solution = Arc::new(AtomicBool::new(false));
        let solution = Arc::new(Mutex::<(KeccakHash, u64)>::new((
            KeccakHash::new_from_array([0; 32]),
            0,
        )));
        let signer = self.signer();
        let pubkey = signer.pubkey();
		let hash_start_time = Instant::now();
        let thread_handles: Vec<_> = (0..threads)
            .map(|i| {
                std::thread::spawn({
                    let found_solution = found_solution.clone();
                    let solution = solution.clone();
                    let mut stdout = stdout();
                    move || {
                        let n = u64::MAX.saturating_div(threads).saturating_mul(i);
                        let mut next_hash: KeccakHash;
                        let mut nonce: u64 = n;
						let mut hash_passes: i64 = 0;
                        loop {
							hash_passes+=1;
                            next_hash = hashv(&[
                                hash.to_bytes().as_slice(),
                                pubkey.to_bytes().as_slice(),
                                nonce.to_le_bytes().as_slice(),
                            ]);
                            if nonce % 30000 == 0 {
                                if found_solution.load(std::sync::atomic::Ordering::Relaxed) {
                                    return;
                                }
                                if n == 0 {
                                    stdout
                                        .write_all(
                                            format!("\r[{}s] {:.0}M passes\t\t", hash_start_time.elapsed().as_secs(), hash_passes/1000000).as_bytes()
                                            // format!("\r{}", next_hash.to_string()).as_bytes()
										)
                                        .ok();
                                }
                            }
                            if next_hash.le(&difficulty) {
								stdout
									.write_all(
										format!("\r{} [{}s] {:.0}M passes [SOLVED]\t\t", next_hash.to_string(), hash_start_time.elapsed().as_secs(), hash_passes/1000000).as_bytes()
									)
									.ok();
                                found_solution.store(true, std::sync::atomic::Ordering::Relaxed);
                                let mut w_solution = solution.lock().expect("failed to lock mutex");
                                *w_solution = (next_hash, nonce);
                                return;
                            }
                            nonce += 1;
                        }
                    }
                })
            })
            .collect();

        for thread_handle in thread_handles {
            thread_handle.join().unwrap();
        }

        let r_solution = solution.lock().expect("Failed to get lock");
        *r_solution
    }

    pub fn validate_hash(
        &self,
        hash: KeccakHash,
        current_hash: KeccakHash,
        signer: Pubkey,
        nonce: u64,
        difficulty: KeccakHash,
    ) -> bool {
        // Validate hash correctness
        let hash_ = hashv(&[
            current_hash.as_ref(),
            signer.as_ref(),
            nonce.to_le_bytes().as_slice(),
        ]);
        if sol_memcmp(hash.as_ref(), hash_.as_ref(), HASH_BYTES) != 0 {
            return false;
        }

        // Validate hash difficulty
        if hash.gt(&difficulty) {
            return false;
        }

        true
    }

	// Lookup the Amount of ORE in the connected wallet
    pub async fn get_ore_display_balance(&self) -> f64 {
        let client = self.rpc_client.clone();
        let signer = self.signer();
        let token_account_address = spl_associated_token_account::get_associated_token_address(
            &signer.pubkey(),
            &ore::MINT_ADDRESS,
        );
        match client.get_token_account(&token_account_address).await {
            Ok(token_account) => {
                if let Some(token_account) = token_account {
                    token_account.token_amount.ui_amount_string.parse().unwrap()
                } else {
                    0.00
                }
            }
            Err(_) => 0.00,
        }
    }

	// Lookup the Amount of SOL in the connected wallet
    pub async fn get_sol_display_balance(&self) -> f64 {
	    let client = self.rpc_client.clone();
        let signer = self.signer();
		match client.get_account(&signer.pubkey()).await {
			Ok(account) => {
				let lamports_balance = account.lamports;							// Extract the SOL balance (in lamports)
				let sol_balance = lamports_balance as f64 / 1_000_000_000.0;		// Convert lamports to SOL (1 SOL = 1_000_000_000 lamports)
				return sol_balance;
			},
			Err(_) => 0.00,
		}
	}

}
