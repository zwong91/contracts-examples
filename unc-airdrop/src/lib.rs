use borsh::{BorshDeserialize, BorshSerialize};
use unc_sdk::store::LookupMap;
use unc_sdk::json_types::U128;
use unc_sdk::{
    env, ext_contract, unc_bindgen, AccountId, Allowance, Gas, PanicOnDefault, Promise, PromiseResult, PublicKey, UncToken
};

mod models;
use models::*;

#[unc_bindgen]
#[derive(PanicOnDefault, BorshDeserialize, BorshSerialize)]
pub struct AirDrop {
    pub accounts: LookupMap<PublicKey, UncToken>,
}

/// Access key allowance for airdrop keys.
const ACCESS_KEY_ALLOWANCE: UncToken = UncToken::from_attounc(1_000_000_000_000_000_000_000_000);

/// Gas attached to the callback from account creation.
pub const ON_CREATE_ACCOUNT_CALLBACK_GAS: Gas = Gas::from_gas(13_000_000_000_000);

/// Methods callable by the function call access key
const ACCESS_KEY_METHOD_NAMES: &str = "claim,create_account_and_claim";

#[ext_contract(ext_self)]
pub trait ExtAirDrop {
    /// Callback after plain account creation.
    fn on_account_created(&mut self, predecessor_account_id: AccountId, amount: U128) -> bool;

    /// Callback after creating account and claiming airdrop.
    fn on_account_created_and_claimed(&mut self, amount: U128) -> bool;
}

fn is_promise_success() -> bool {
    assert_eq!(
        env::promise_results_count(),
        1,
        "Contract expected a result on the callback"
    );
    match env::promise_result(0) {
        PromiseResult::Successful(_) => true,
        _ => false,
    }
}

#[unc_bindgen]
impl AirDrop {
    /// Initializes the contract with an empty map for the accounts
    #[init]
    pub fn new() -> Self {
        Self { 
            accounts: LookupMap::new(b"a") 
        }
    }

    /// Allows given public key to claim sent balance.
    /// Takes ACCESS_KEY_ALLOWANCE as fee from deposit to cover account creation via an access key.
    #[payable]
    pub fn send(&mut self, public_key: PublicKey) -> Promise {
        assert!(
            env::attached_deposit() > ACCESS_KEY_ALLOWANCE,
            "Attached deposit must be greater than ACCESS_KEY_ALLOWANCE"
        );
        let pk = public_key.into();
        let zero = UncToken::from_unc(0);
        let value = self.accounts.get(&pk).unwrap_or(&zero);
        self.accounts.insert(
            pk.to_owned(),
            value.saturating_add(env::attached_deposit()).saturating_sub(ACCESS_KEY_ALLOWANCE),
        );
        Promise::new(env::current_account_id()).add_access_key_allowance(
            pk,
            Allowance::limited(ACCESS_KEY_ALLOWANCE).unwrap_or(Allowance::Unlimited),
            env::current_account_id(),
            ACCESS_KEY_METHOD_NAMES.to_string(),
        )
    }

    /// Claim tokens for specific account that are attached to the public key this tx is signed with.
    pub fn claim(&mut self, account_id: AccountId) -> Promise {
        assert_eq!(
            env::predecessor_account_id(),
            env::current_account_id(),
            "Claim only can come from this account"
        );
        assert!(
            env::is_valid_account_id(account_id.as_bytes()),
            "Invalid account id"
        );
        let amount = self
            .accounts
            .remove(&env::signer_account_pk())
            .expect("Unexpected public key");
        Promise::new(env::current_account_id()).delete_key(env::signer_account_pk());
        Promise::new(account_id).transfer(amount)
    }

    /// Create new account and and claim tokens to it.
    pub fn create_account_and_claim(
        &mut self,
        new_account_id: AccountId,
        new_public_key: PublicKey,
    ) -> Promise {
        assert_eq!(
            env::predecessor_account_id(),
            env::current_account_id(),
            "Create account and claim only can come from this account"
        );
        assert!(
            env::is_valid_account_id(new_account_id.as_bytes()),
            "Invalid account id"
        );
        let amount = self
            .accounts
            .remove(&env::signer_account_pk())
            .expect("Unexpected public key");
        Promise::new(new_account_id)
            .create_account()
            .add_full_access_key(new_public_key.into())
            .transfer(amount)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(ON_CREATE_ACCOUNT_CALLBACK_GAS)
                    .on_account_created_and_claimed(amount.into())
            )
    }

    /// Create new account without airdrop and deposit passed funds (used for creating sub accounts directly).
    #[payable]
    pub fn create_account(
        &mut self,
        new_account_id: AccountId,
        new_public_key: PublicKey,
    ) -> Promise {
        assert!(
            env::is_valid_account_id(new_account_id.as_bytes()),
            "Invalid account id"
        );
        let amount = env::attached_deposit();
        Promise::new(new_account_id)
            .create_account()
            .add_full_access_key(new_public_key.into())
            .transfer(amount)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(ON_CREATE_ACCOUNT_CALLBACK_GAS)
                    .on_account_created(
                        env::predecessor_account_id(),
                        amount.into()
                    )
            )
    }

    /// Create new account without airdrop and deposit passed funds (used for creating sub accounts directly).
    #[payable]
    pub fn create_account_advanced(
        &mut self,
        new_account_id: AccountId,
        options: CreateAccountOptions,
    ) -> Promise {
        let is_some_option = options.contract_bytes.is_some() || options.full_access_keys.is_some() || options.limited_access_keys.is_some();
        assert!(is_some_option, "Cannot create account with no options. Please specify either contract bytes, full access keys, or limited access keys.");

        let amount = env::attached_deposit();

        // Initiate a new promise on the new account we're creating and transfer it any attached deposit
        let mut promise = Promise::new(new_account_id).create_account().transfer(amount);
        
        // If there are any full access keys in the options, loop through and add them to the promise
        if let Some(full_access_keys) = options.full_access_keys {
            for key in full_access_keys {
                promise = promise.add_full_access_key(key.clone());
            }
        }

        // If there are any function call access keys in the options, loop through and add them to the promise
        if let Some(limited_access_keys) = options.limited_access_keys {
            for key_info in limited_access_keys {
                promise = promise.add_access_key_allowance(key_info.public_key.clone(), Allowance::limited(key_info.allowance).unwrap_or(Allowance::Unlimited), key_info.receiver_id.clone(), key_info.method_names.clone());
            }
        }

        // If there are any contract bytes, we should deploy the contract to the account
        if let Some(bytes) = options.contract_bytes {
            promise = promise.deploy_contract(bytes);
        };

        // Callback if anything went wrong, refund the predecessor for their attached deposit
        promise.then(
            Self::ext(env::current_account_id())
                .with_static_gas(ON_CREATE_ACCOUNT_CALLBACK_GAS)
                .on_account_created(
                    env::predecessor_account_id(),
                    amount.into()
                )
        )
    }

    /// Callback after executing `create_account` or `create_account_advanced`.
    pub fn on_account_created(&mut self, predecessor_account_id: AccountId, amount: UncToken) -> bool {
        assert_eq!(
            env::predecessor_account_id(),
            env::current_account_id(),
            "Callback can only be called from the contract"
        );
        let creation_succeeded = is_promise_success();
        if !creation_succeeded {
            // In case of failure, send funds back.
            Promise::new(predecessor_account_id).transfer(amount.into());
        }
        creation_succeeded
    }

    /// Callback after execution `create_account_and_claim`.
    pub fn on_account_created_and_claimed(&mut self, amount: UncToken) -> bool {
        assert_eq!(
            env::predecessor_account_id(),
            env::current_account_id(),
            "Callback can only be called from the contract"
        );
        let creation_succeeded = is_promise_success();
        if creation_succeeded {
            Promise::new(env::current_account_id()).delete_key(env::signer_account_pk());
        } else {
            // In case of failure, put the amount back.
            self.accounts
                .insert(env::signer_account_pk(), amount.into());
        }
        creation_succeeded
    }

    /// Returns the balance associated with given key.
    pub fn get_key_balance(&self, key: PublicKey) -> &UncToken {
        self.accounts.get(&key.into()).expect("Key is missing").into()
    }

    /// Returns information associated with a given key.
    /// Part of the airdrop NEP
    #[handle_result]
    pub fn get_key_information(&self, key: PublicKey) -> Result<KeyInfo, &'static str> {
        match self.accounts.get(&key) {
            Some(balance) => Ok(KeyInfo { balance: U128::from(balance.as_attounc()) }),
            None => Err("Key is missing"),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {

    use super::*;

    use unc_sdk::test_utils::VMContextBuilder;
    use unc_sdk::testing_env;

    fn airdrop() -> AccountId {
        "airdrop".parse().unwrap()
    }

    fn bob() -> AccountId {
        "bob".parse().unwrap()
    }

    #[test]
    fn test_create_account() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();
        // Default the deposit to an extremely small amount
        let deposit = 1_000_000;

        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Create bob's account with the PK
        contract.create_account(bob(), pk);
    }

    #[test]
    #[should_panic]
    fn test_create_invalid_account() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();
        // Default the deposit to an extremely small amount
        let deposit = 1_000_000;

        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Attempt to create an invalid account with the PK
        contract.create_account("XYZ".parse().unwrap(), pk);
    }

    #[test]
    #[should_panic]
    fn test_get_missing_balance_panics() {
        // Create a new instance of the airdrop contract
        let contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();

        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .context.clone()
        );

        contract.get_key_balance(pk);
    }

    #[test]
    fn test_get_missing_balance_success() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();
        // Default the deposit to be 100 times the access key allowance
        let deposit = ACCESS_KEY_ALLOWANCE.saturating_mul(100);
        
        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Create the airdrop
        contract.send(pk.clone());

        // try getting the balance of the key
        let balance:u128 = contract.get_key_balance(pk).as_attounc();
        assert_eq!(
            balance,
            u128::from((deposit.saturating_sub(ACCESS_KEY_ALLOWANCE)).as_attounc())
        );
    }

    #[test]
    #[should_panic]
    fn test_claim_invalid_account() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();
        // Default the deposit to be 100 times the access key allowance
        let deposit = ACCESS_KEY_ALLOWANCE.saturating_mul(100);
        
        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Create the airdrop
        contract.send(pk.clone());

        // Now, send new transaction to airdrop contract and reinitialize the mocked blockchain with new params
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .predecessor_account_id(airdrop())
            .signer_account_pk(pk.into())
            .account_balance(deposit)
            .context.clone()
        );

        // Create the second public key
        let pk2 = "2S87aQ1PM9o6eBcEXnTR5yBAVRTiNmvj8J8ngZ6FzSca"
            .parse()
            .unwrap();
        // Attempt to create the account and claim
        contract.create_account_and_claim("XYZ".parse().unwrap(), pk2);
    }

    #[test]
    fn test_drop_claim() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();
        // Default the deposit to be 100 times the access key allowance
        let deposit = ACCESS_KEY_ALLOWANCE.saturating_mul(100);
        
        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Create the airdrop
        contract.send(pk.clone());

        // Now, send new transaction to airdrop contract and reinitialize the mocked blockchain with new params
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .predecessor_account_id(airdrop())
            .signer_account_pk(pk.into())
            .account_balance(deposit)
            .context.clone()
        );

        // Create the second public key
        let pk2 = "2S87aQ1PM9o6eBcEXnTR5yBAVRTiNmvj8J8ngZ6FzSca"
            .parse()
            .unwrap();
        // Attempt to create the account and claim
        contract.create_account_and_claim(bob(), pk2);
    }

    #[test]
    fn test_send_two_times() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();
        // Default the deposit to be 100 times the access key allowance
        let deposit = ACCESS_KEY_ALLOWANCE.saturating_mul(100);
        
        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Create the airdrop
        contract.send(pk.clone());
        assert_eq!(contract.get_key_balance(pk.clone()), (deposit.saturating_sub(ACCESS_KEY_ALLOWANCE)).into());

        // Re-initialize the mocked blockchain with new params
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .account_balance(deposit)
            .attached_deposit(deposit + 1)
            .context.clone()
        );

        // Attempt to recreate the same airdrop twice
        contract.send(pk.clone());
        assert_eq!(
            contract.accounts.get(&pk.into()).unwrap().as_attounc(),
            deposit.as_attounc() + deposit.as_attounc() + 1 - 2 * ACCESS_KEY_ALLOWANCE.as_attounc()
        );
    }

    #[test]
    fn test_create_advanced_account() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Create the public key to be used in the test
        let pk: PublicKey = "qSq3LoufLvTCTNGC3LJePMDGrok8dHMQ5A1YD9psbiz"
            .parse()
            .unwrap();
        // Default the deposit to an extremely small amount
        let deposit = 1_000_000;

        // Create options for the advanced account creation
        let options: CreateAccountOptions = CreateAccountOptions {
            full_access_keys: Some(vec![pk.clone()]),
            limited_access_keys: Some(vec![LimitedAccessKey {
                public_key: pk.clone(),
                allowance: UncToken::from_attounc(100),
                receiver_id: airdrop(),
                method_names: "send".to_string(),
            }]),
            contract_bytes: Some(include_bytes!("../target/wasm32-unknown-unknown/release/airdrop.wasm").to_vec()),
        };

        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Create bob's account with the advanced options
        contract.create_account_advanced(bob(), options);
    }

    #[test]
    #[should_panic]
    fn test_create_advanced_account_no_options() {
        // Create a new instance of the airdrop contract
        let mut contract = AirDrop::new();
        // Default the deposit to an extremely small amount
        let deposit = 1_000_000;

        // Initialize the mocked blockchain
        testing_env!(
            VMContextBuilder::new()
            .current_account_id(airdrop())
            .attached_deposit(deposit)
            .context.clone()
        );

        // Create bob's account with the advanced options
        contract.create_account_advanced(bob(), CreateAccountOptions { full_access_keys: None, limited_access_keys: None, contract_bytes: None });
    }
}
