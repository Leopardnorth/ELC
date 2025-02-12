#![cfg_attr(not(feature = "std"), no_std)]

pub use self::pool::Pool;
use ink_lang as ink;

#[ink::contract]
mod pool {
    use elc::ELC;
    use rELP::RELP;
    use oracle::Oracle;
    #[cfg(not(feature = "ink-as-dependency"))]
    use ink_env::call::FromAccountId;
    #[cfg(not(feature = "ink-as-dependency"))]
    use ink_storage::Lazy;

    #[ink(storage)]
    pub struct Pool {
        elcaim: u128,
        k: u128, //inflation factor
        reserve: Balance,
        risk_reserve: Balance,
        k_update_time: u128,
        elc_contract: Lazy<ELC>,
        rELP_contract: Lazy<RELP>,
        oracle_contract: Lazy<Oracle>,
    }

    impl Pool {
        #[ink(constructor)]
        pub fn new(
            reserve: Balance,
            risk_reserve: Balance,
            elc_token: AccountId,
            rELP_token: AccountId,
            oracle_addr: AccountId,
        ) -> Self {
            let elc_contract: ELC = FromAccountId::from_account_id(elc_token);
            let rELP_contract: RELP = FromAccountId::from_account_id(rELP_token);
            let oracle_contract: Oracle = FromAccountId::from_account_id(oracle_addr);
            let instance = Self {
                elcaim: 1,
                k: 5, //0.00005 * 100000
                reserve: reserve,
                risk_reserve: risk_reserve,
                k_update_time: Self::env().block_timestamp().into(),
                oracle_contract: Lazy::new(oracle_contract),
                elc_contract: Lazy::new(elc_contract),
                rELP_contract: Lazy::new(rELP_contract),
            };
            instance
        }

        /// 增加流动性(ELP)，返回rELP和ELC
        #[ink(message, payable)]
        pub fn add_liquidity(&mut self) -> (Balance, Balance) {
            self.update_ELCaim(); //首先更新ELCaim价格
            let LR = self.liability_ratio(); //计算LR
            let caller: AccountId = self.env().caller();
            let elp_amount: u128 = self.env().transferred_balance();
            let elp_price: u128 = self.oracle_contract.elp_price();
            let elc_price: u128 = self.oracle_contract.elc_price();
            let mut rELP_tokens: Balance = 0;
            let mut elc_tokens: Balance = 0;
            let rELP_balance = self.rELP_contract.total_supply();
            let rELP_price = elp_price * self.reserve / rELP_balance;
            if LR > 30 {
                //返回用户rELP和 0 ELC
                let elc_tokens = elp_price * elp_amount * (LR/100000) / rELP_price;
                assert!(self
                    .rELP_contract
                    .mint(caller, elc_tokens)
                    .is_ok());

                let rELP_tokens = elp_price * elp_amount * (1- LR/100000)/ rELP_price;
                assert!(self
                    .rELP_contract
                    .mint(caller, rELP_tokens)
                    .is_ok());
            } else {
                //返回用户ELC和rELP数量
                let rELP_tokens = elp_price * elp_amount / rELP_price;
                assert!(self
                    .rELP_contract
                    .mint(caller, rELP_tokens)
                    .is_ok());
            };
            (rELP_tokens, elc_tokens)
        }

        /// 退出流动性，发送ELP给用户,赎回只能使用rELP，
        #[ink(message)]
        pub fn remove_liquidity(&mut self, rELP_amount: Balance) -> Balance {
            let caller: AccountId= self.env().caller();
            let elp_price: u128 = self.oracle_contract.elp_price();
            let LR = self.liability_ratio(); //计算LR
            let rELP_balance = self.rELP_contract.total_supply();
            let mut elp_amount: u128 = 0;
            assert!(rELP_amount > 0);
            //burn rELP
            assert!(self
                .rELP_contract
                .burn(caller, rELP_amount)
                .is_ok());

            //正向兑换rELP时 LR>30，ELP仅兑换rELP，反向兑亦然
            if LR > 30 {
                //compute ELP amount
                //△Amount(ELP) = △Amount(rELP) * p(rELP) / p(ELP)
                // △Amount(ELP) = △Amount(rELP)*Amount(ELP)/Amount(rELP)
                let elp_amount = rELP_amount * self.reserve / rELP_balance;
            } else {
                //△Amount(ELP) = △Amount(rELP) * p(rELP) / (p(ELP) * (1-LR))
                // △Amount(ELP) = △Amount(rELP)*Amount(ELP)/Amount(rELP) / (1-LR))
                let elp_amount =  rELP_amount * self.reserve / rELP_balance / (1 - LR/100000);
            }

            //redeem ELP
            assert!(self.env().transfer(caller, elp_amount).is_ok());

            //give reward
            self.get_reward();
            elp_amount
        }

        /// 持有rELP即可领取奖励
        #[ink(message)]
        pub fn get_reward(&mut self) -> Balance {
            let caller: AccountId= self.env().caller();
            let rELP_amount = self.rELP_contract.balance_of(caller);
            assert!(rELP_amount > 0);
            //返回ELP数量
            rELP_amount
        }

        /// 扩张，swap选择交易所待定，提供给外部做市商调用，保证每次小量交易
        #[ink(message)]
        pub fn expand_elc(&mut self) {
            let elc_price: u128 = self.oracle_contract.elc_price();
            let elcaim = self.elcaim;
            assert!(elc_price < elcaim * 98 / 100);
            //调用swap，卖出ELC，买入ELP
        }

        /// 收缩
        #[ink(message)]
        pub fn contract_elc(&mut self){
            let elc_price: u128 = self.oracle_contract.elc_price();
            let elcaim = self.elcaim;
            assert!(elc_price > elcaim * 102 / 100);
            //调用swap，卖出ELP，买入ELC
        }

        ///计算通胀因子，如果通胀因子变动要更新, 出块速度为6秒/块，每隔10000个块将ELC目标价格调升K
        #[ink(message)]
        pub fn update_ELCaim(&mut self) {
            let block_time:u128 = self.env().block_timestamp().into();
            let elcaim:u128 = self.elcaim;
            let last_update_time = self.k_update_time;
            let k: u128 = (block_time - self.k_update_time) / 6 / 10000;
            if k > 0 {
                self.elcaim = elcaim * (100000 + k) / 100000;
                self.k_update_time = last_update_time + (k * 10000 * 6);
            }
        }

        /// 返回系统负债率，调用时需要实时计算, 返回整数，以100为基数
        #[ink(message)]
        pub fn liability_ratio(&self) -> u128 {
            let elp_price: u128 = self.oracle_contract.elp_price();
            let elc_price: u128 = self.oracle_contract.elc_price();
            let elp_amount: Balance = self.reserve;
            let elc_amount: Balance = self.elc_contract.total_supply();
            let lr =  elc_amount * elc_price/(elp_price * elp_amount) * 100; //100为精度
            lr
        }

        #[ink(message)]
        pub fn elp_reserve(&self) -> Balance { self.reserve }

        #[ink(message)]
        pub fn elp_risk_reserve(&self) -> Balance { self.risk_reserve }
    }
}
