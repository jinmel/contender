use crate::db::database::DbOps;
use crate::error::ContenderError;
pub use crate::generator::types::TestConfig;
use crate::generator::{
    templater::Templater,
    types::{CreateDefinition, FunctionCallDefinition, RpcProvider},
    PlanConfig,
};
use crate::spammer::OnTxSent;
use alloy::hex::ToHexExt;
use alloy::primitives::{Address, TxHash};
use std::collections::HashMap;
use std::fs::read;
use std::sync::Arc;
use tokio::task::{spawn as spawn_task, JoinHandle};

use super::NamedTxRequest;

impl TestConfig {
    pub fn from_file(file_path: &str) -> Result<TestConfig, Box<dyn std::error::Error>> {
        let file_contents = read(file_path)?;
        let file_contents_str = String::from_utf8_lossy(&file_contents).to_string();
        let test_file: TestConfig = toml::from_str(&file_contents_str)?;
        Ok(test_file)
    }

    pub fn encode_toml(&self) -> Result<String, Box<dyn std::error::Error>> {
        let encoded = toml::to_string(self)?;
        Ok(encoded)
    }

    pub fn save_toml(&self, file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let encoded = self.encode_toml()?;
        std::fs::write(file_path, encoded)?;
        Ok(())
    }
}

impl PlanConfig<String> for TestConfig {
    fn get_spam_steps(&self) -> Result<Vec<FunctionCallDefinition>, ContenderError> {
        self.spam
            .to_owned()
            .ok_or(ContenderError::SpamError("no spam steps found", None))
    }

    fn get_setup_steps(&self) -> Result<Vec<FunctionCallDefinition>, ContenderError> {
        self.setup
            .to_owned()
            .ok_or(ContenderError::SetupError("no setup steps found", None))
    }

    fn get_create_steps(&self) -> Result<Vec<CreateDefinition>, ContenderError> {
        self.create
            .to_owned()
            .ok_or(ContenderError::SetupError("no create steps found", None))
    }

    fn get_env(&self) -> Result<HashMap<String, String>, ContenderError> {
        self.env.to_owned().ok_or(ContenderError::SetupError(
            "no environment variables found",
            None,
        ))
    }
}

impl Templater<String> for TestConfig {
    /// Find values wrapped in brackets in a string and replace them with values from a hashmap whose key match the value in the brackets.
    /// example: "hello {world}" with hashmap {"world": "earth"} will return "hello earth"
    fn replace_placeholders(&self, input: &str, template_map: &HashMap<String, String>) -> String {
        let mut output = input.to_owned();
        for (key, value) in template_map.iter() {
            let template = format!("{{{}}}", key);
            output = output.replace(&template, value);
        }
        output
    }

    fn terminator_start(&self, input: &str) -> Option<usize> {
        input.find("{")
    }

    fn terminator_end(&self, input: &str) -> Option<usize> {
        input.find("}")
    }

    fn num_placeholders(&self, input: &str) -> usize {
        input.chars().filter(|&c| c == '{').count()
    }

    fn copy_end(&self, input: &str, last_end: usize) -> String {
        input.split_at(last_end).1.to_owned()
    }

    fn find_key(&self, input: &str) -> Option<(String, usize)> {
        if let Some(template_start) = self.terminator_start(input) {
            let template_end = self.terminator_end(input);
            if let Some(template_end) = template_end {
                let template_name = &input[template_start + 1..template_end];
                return Some((template_name.to_owned(), template_end));
            }
        }
        None
    }

    fn encode_contract_address(&self, input: &Address) -> String {
        input.encode_hex()
    }
}
pub struct NilCallback;

impl NilCallback {
    pub fn new() -> Self {
        Self {}
    }
}

pub struct LogCallback<D> {
    pub db: Arc<D>,
    pub rpc_provider: Arc<RpcProvider>,
}

impl<D> LogCallback<D>
where
    D: DbOps + Send + Sync + 'static,
{
    pub fn new(db: Arc<D>, rpc_provider: Arc<RpcProvider>) -> Self {
        Self { db, rpc_provider }
    }
}

impl OnTxSent for NilCallback {
    fn on_tx_sent(
        &self,
        _tx_res: TxHash,
        _req: NamedTxRequest,
        _extra: Option<HashMap<String, String>>,
    ) -> Option<JoinHandle<()>> {
        // do nothing
        None
    }
}

impl<D> OnTxSent for LogCallback<D>
where
    D: DbOps + Send + Sync + 'static,
{
    fn on_tx_sent(
        &self,
        tx_hash: TxHash,
        _req: NamedTxRequest,
        extra: Option<HashMap<String, String>>,
    ) -> Option<JoinHandle<()>> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("failed to get timestamp")
            .as_millis() as usize;
        let db = self.db.clone();
        let run_id = extra
            .map(|e| e.get("run_id").unwrap().parse::<u64>().unwrap())
            .unwrap_or(0);
        let handle = spawn_task(async move {
            db.insert_run_tx(run_id, tx_hash, timestamp)
                .expect("failed to insert tx into db");
        });
        Some(handle)
    }
}

#[cfg(test)]
pub mod tests {
    use super::TestConfig;
    use crate::db::sqlite::SqliteDb;
    use crate::generator::{
        types::{CreateDefinition, FunctionCallDefinition, FuzzParam, PlanType},
        util::test::spawn_anvil,
        Generator, RandSeed,
    };
    use crate::test_scenario::TestScenario;
    use alloy::primitives::U256;
    use alloy::signers::local::PrivateKeySigner;
    use alloy::{hex::ToHexExt, primitives::Address};
    use std::str::FromStr;
    use std::{collections::HashMap, fs};

    pub fn get_test_signers() -> Vec<PrivateKeySigner> {
        vec![
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
            "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
        ]
        .iter()
        .map(|s| PrivateKeySigner::from_str(s).unwrap())
        .collect::<Vec<PrivateKeySigner>>()
    }

    pub fn get_testconfig() -> TestConfig {
        TestConfig {
            env: None,
            create: None,
            setup: None,
            spam: vec![FunctionCallDefinition {
                to: "0x7a250d5630B4cF539739dF2C5dAcb4c659F248DD".to_owned(),
                from: "0x7a250d5630B4cF539739dF2C5dAcb4c659F248DD".to_owned(),
                signature: "swap(uint256 x, uint256 y, address a, bytes b)".to_owned(),
                args: vec![
                    "1".to_owned(),
                    "2".to_owned(),
                    Address::repeat_byte(0x11).encode_hex(),
                    "0xdead".to_owned(),
                ]
                .into(),
                fuzz: None,
                value: None,
            }]
            .into(),
        }
    }

    pub fn get_fuzzy_testconfig() -> TestConfig {
        let fn_call = |data: &str, from_addr: &str| FunctionCallDefinition {
            to: "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_owned(),
            from: from_addr.to_owned(),
            value: None,
            signature: "swap(uint256 x, uint256 y, address a, bytes b)".to_owned(),
            args: vec![
                "1".to_owned(),
                "2".to_owned(),
                Address::repeat_byte(0x11).encode_hex(),
                data.to_owned(),
            ]
            .into(),
            fuzz: vec![FuzzParam {
                param: "x".to_string(),
                min: None,
                max: None,
            }]
            .into(),
        };
        TestConfig {
            env: None,
            create: None,
            setup: None,
            spam: vec![
                fn_call("0xbeef", "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
                fn_call("0xea75", "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"),
                fn_call("0xf00d", "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"),
            ]
            .into(),
        }
    }

    pub fn get_setup_testconfig() -> TestConfig {
        TestConfig {
            env: None,
            create: None,
            spam: None,
            setup: vec![
                FunctionCallDefinition {
                    to: "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_owned(),
                    from: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_owned(),
                    value: Some("4096".to_owned()),
                    signature: "swap(uint256 x, uint256 y, address a, bytes b)".to_owned(),
                    args: vec![
                        "1".to_owned(),
                        "2".to_owned(),
                        Address::repeat_byte(0x11).encode_hex(),
                        "0xdead".to_owned(),
                    ]
                    .into(),
                    fuzz: None,
                },
                FunctionCallDefinition {
                    to: "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_owned(),
                    from: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_owned(),
                    value: Some("0x1000".to_owned()),
                    signature: "swap(uint256 x, uint256 y, address a, bytes b)".to_owned(),
                    args: vec![
                        "1".to_owned(),
                        "2".to_owned(),
                        Address::repeat_byte(0x11).encode_hex(),
                        "0xbeef".to_owned(),
                    ]
                    .into(),
                    fuzz: None,
                },
            ]
            .into(),
        }
    }

    pub const COUNTER_BYTECODE: &'static str =
        "0x608060405234801561001057600080fd5b5060f78061001f6000396000f3fe6080604052348015600f57600080fd5b5060043610603c5760003560e01c80633fb5c1cb1460415780638381f58a146053578063d09de08a14606d575b600080fd5b6051604c3660046083565b600055565b005b605b60005481565b60405190815260200160405180910390f35b6051600080549080607c83609b565b9190505550565b600060208284031215609457600080fd5b5035919050565b60006001820160ba57634e487b7160e01b600052601160045260246000fd5b506001019056fea264697066735822122010f3077836fb83a22ad708a23102f2b487523767e1afef5a93c614619001648b64736f6c63430008170033";

    pub fn get_create_testconfig() -> TestConfig {
        let mut env = HashMap::new();
        env.insert("test1".to_owned(), "0xbeef".to_owned());
        env.insert("test2".to_owned(), "0x9001".to_owned());
        TestConfig {
            env: Some(env),
            create: Some(vec![CreateDefinition {
                bytecode: COUNTER_BYTECODE.to_string(),
                name: "test_counter".to_string(),
                from: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_owned(),
            }]),
            spam: None,
            setup: None,
        }
    }

    pub fn get_composite_testconfig() -> TestConfig {
        let tc_fuzz = get_fuzzy_testconfig();
        let tc_setup = get_setup_testconfig();
        let tc_create = get_create_testconfig();
        TestConfig {
            env: tc_create.env, // TODO: add something here
            create: tc_create.create,
            spam: tc_fuzz.spam,
            setup: tc_setup.setup,
        }
    }

    #[test]
    fn parses_testconfig_toml() {
        let test_file = TestConfig::from_file("univ2ConfigTest.toml").unwrap();
        assert!(test_file.env.is_some());
        assert!(test_file.setup.is_some());
        assert!(test_file.spam.is_some());
        let env = test_file.env.unwrap();
        let setup = test_file.setup.unwrap();
        let spam = test_file.spam.unwrap();

        assert_eq!(
            env.get("feeToSetter").unwrap(),
            "f39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
        );
        assert_eq!(
            spam[0].from,
            "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".to_owned()
        );
        assert_eq!(setup.len(), 11);
        assert_eq!(setup[0].value, Some("10000000000000000000".to_owned()));
        assert_eq!(spam[0].fuzz.as_ref().unwrap()[0].param, "amountIn");
        assert_eq!(
            spam[1].fuzz.as_ref().unwrap()[0].min,
            Some(U256::from(100000000))
        );
    }

    fn print_testconfig(cfg: &str) {
        println!("{}", "-".repeat(80));
        println!("{}", cfg);
        println!("{}", "-".repeat(80));
    }

    #[test]
    fn encodes_testconfig_toml() {
        let cfg = get_composite_testconfig();
        let encoded = cfg.encode_toml().unwrap();
        print_testconfig(&encoded);
        cfg.save_toml("cargotest.toml").unwrap();
        let test_file2 = TestConfig::from_file("cargotest.toml").unwrap();
        let spam = cfg.clone().spam.unwrap();
        let args = spam[0].args.as_ref().unwrap();
        assert_eq!(spam[0].to, test_file2.spam.unwrap()[0].to);
        assert_eq!(args[0], "1");
        assert_eq!(args[1], "2");
        fs::remove_file("cargotest.toml").unwrap();
    }

    #[tokio::test]
    async fn gets_spam_txs() {
        let anvil = spawn_anvil();
        let test_file = get_testconfig();
        let seed = RandSeed::new();
        let test_gen = TestScenario::new(
            test_file,
            SqliteDb::new_memory().into(),
            anvil.endpoint_url(),
            seed,
            &get_test_signers(),
        );
        // this seed can be used to recreate the same test tx(s)
        let spam_txs = test_gen
            .load_txs(PlanType::Spam(10, |_tx_req| {
                println!(
                    "spam tx\n\tfrom={:?}\n\tto={:?}\n\tinput={:?}",
                    _tx_req.tx.from, _tx_req.tx.to, _tx_req.tx.input.input
                );
                Ok(None)
            }))
            .await
            .unwrap();
        assert_eq!(spam_txs.len(), 10);
        let data = spam_txs[0].tx.input.input.to_owned().unwrap().to_string();
        assert_eq!(data, "0x022c0d9f00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000111111111111111111111111111111111111111100000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000002dead000000000000000000000000000000000000000000000000000000000000");
    }

    #[tokio::test]
    async fn fuzz_is_deterministic() {
        let anvil = spawn_anvil();
        let test_file = get_fuzzy_testconfig();
        let seed = RandSeed::from_bytes(&[0x01; 32]);
        let signers = get_test_signers();
        let scenario1 = TestScenario::new(
            test_file.clone(),
            SqliteDb::new_memory().into(),
            anvil.endpoint_url(),
            seed.to_owned(),
            &signers,
        );
        let scenario2 = TestScenario::new(
            test_file,
            SqliteDb::new_memory().into(),
            anvil.endpoint_url(),
            seed,
            &signers,
        );

        let num_txs = 13;
        let spam_txs_1 = scenario1
            .load_txs(PlanType::Spam(num_txs, |_| Ok(None)))
            .await
            .unwrap();
        let spam_txs_2 = scenario2
            .load_txs(PlanType::Spam(num_txs, |_| Ok(None)))
            .await
            .unwrap();
        assert_eq!(spam_txs_1.len(), spam_txs_2.len());
        for i in 0..spam_txs_1.len() {
            let data1 = spam_txs_1[i].tx.input.input.to_owned().unwrap().to_string();
            let data2 = spam_txs_2[i].tx.input.input.to_owned().unwrap().to_string();
            assert_eq!(data1, data2);
        }
    }
}
