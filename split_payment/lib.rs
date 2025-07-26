#![cfg_attr(not(feature = "std"), no_std)]

use ink::prelude::vec::Vec;
use ink::prelude::collections::BTreeMap;

#[ink::contract]
mod split_payment {
    use super::*;
    use ink::storage::Mapping;

    /// Errors that can occur in the contract
    #[derive(Debug, PartialEq, Eq)]
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    pub enum Error {
        /// Caller is not authorized to perform this action
        Unauthorized,
        /// Insufficient balance for the operation
        InsufficientBalance,
        /// Insufficient allowance for the operation
        InsufficientAllowance,
        /// Invalid beneficiary (zero address or already exists)
        InvalidBeneficiary,
        /// Invalid share percentage (must be > 0 and total <= 100)
        InvalidShare,
        /// No funds available to withdraw
        NoFundsAvailable,
        /// Transfer failed
        TransferFailed,
        /// Beneficiary not found
        BeneficiaryNotFound,
        /// Contract is paused
        ContractPaused,
    }

    /// Result type for contract operations
    pub type Result<T> = core::result::Result<T, Error>;

    /// Beneficiary information
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Beneficiary {
        pub account: AccountId,
        pub share_percentage: u8, // 0-100
        pub pending_balance: Balance,
        pub total_withdrawn: Balance,
    }

    /// Approval information for spending allowance
    #[ink::scale_derive(Encode, Decode, TypeInfo)]
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Approval {
        pub spender: AccountId,
        pub amount: Balance,
        pub expires_at: Option<u64>, // Optional expiration timestamp
    }

    #[ink(storage)]
    pub struct SplitPayment {
        /// Contract owner (has admin privileges)
        owner: AccountId,
        /// List of authorized managers (can add/remove beneficiaries)
        managers: Mapping<AccountId, bool>,
        /// List of beneficiaries and their share percentages
        beneficiaries: Vec<Beneficiary>,
        /// Total share percentage allocated (should not exceed 100)
        total_shares: u8,
        /// Mapping from beneficiary to approvals granted to other accounts
        approvals: Mapping<(AccountId, AccountId), Approval>,
        /// Mapping from account to total allowance they can spend on behalf of others
        allowances: Mapping<AccountId, Balance>,
        /// Contract pause state (emergency stop)
        paused: bool,
        /// Total funds received by the contract
        total_received: Balance,
        /// Total funds distributed
        total_distributed: Balance,
    }

    /// Events emitted by the contract
    #[ink(event)]
    pub struct FundsReceived {
        #[ink(topic)]
        from: AccountId,
        amount: Balance,
    }

    #[ink(event)]
    pub struct FundsDistributed {
        total_amount: Balance,
        beneficiary_count: u32,
    }

    #[ink(event)]
    pub struct BeneficiaryAdded {
        #[ink(topic)]
        beneficiary: AccountId,
        share_percentage: u8,
        #[ink(topic)]
        added_by: AccountId,
    }

    #[ink(event)]
    pub struct BeneficiaryRemoved {
        #[ink(topic)]
        beneficiary: AccountId,
        #[ink(topic)]
        removed_by: AccountId,
    }

    #[ink(event)]
    pub struct ApprovalGranted {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        spender: AccountId,
        amount: Balance,
        expires_at: Option<u64>,
    }

    #[ink(event)]
    pub struct ApprovalRevoked {
        #[ink(topic)]
        owner: AccountId,
        #[ink(topic)]
        spender: AccountId,
    }

    #[ink(event)]
    pub struct WithdrawalByApproval {
        #[ink(topic)]
        beneficiary: AccountId,
        #[ink(topic)]
        spender: AccountId,
        amount: Balance,
    }

    #[ink(event)]
    pub struct ManagerAdded {
        #[ink(topic)]
        manager: AccountId,
        #[ink(topic)]
        added_by: AccountId,
    }

    #[ink(event)]
    pub struct ManagerRemoved {
        #[ink(topic)]
        manager: AccountId,
        #[ink(topic)]
        removed_by: AccountId,
    }

    #[ink(event)]
    pub struct ContractPaused {
        #[ink(topic)]
        by: AccountId,
    }

    #[ink(event)]
    pub struct ContractUnpaused {
        #[ink(topic)]
        by: AccountId,
    }

    impl SplitPayment {
        /// Constructor - creates a new split payment contract
        #[ink(constructor)]
        pub fn new() -> Self {
            let owner = Self::env().caller();
            
            Self {
                owner,
                managers: Mapping::default(),
                beneficiaries: Vec::new(),
                total_shares: 0,
                approvals: Mapping::default(),
                allowances: Mapping::default(),
                paused: false,
                total_received: 0,
                total_distributed: 0,
            }
        }

        /// Payable function to receive funds
        #[ink(message)]
        #[ink(payable)]
        pub fn receive_payment(&mut self) -> Result<()> {
            self.ensure_not_paused()?;
            
            let amount = self.env().transferred_value();
            let caller = self.env().caller();
            
            self.total_received = self.total_received.saturating_add(amount);
            
            // Distribute the received funds immediately
            self.distribute_funds(amount)?;
            
            self.env().emit_event(FundsReceived {
                from: caller,
                amount,
            });
            
            Ok(())
        }

        /// Add a new beneficiary (only owner or managers)
        #[ink(message)]
        pub fn add_beneficiary(&mut self, account: AccountId, share_percentage: u8) -> Result<()> {
            self.ensure_not_paused()?;
            self.ensure_manager_or_owner()?;
            
            if account == AccountId::from([0u8; 32]) {
                return Err(Error::InvalidBeneficiary);
            }
            
            if share_percentage == 0 || self.total_shares.saturating_add(share_percentage) > 100 {
                return Err(Error::InvalidShare);
            }
            
            // Check if beneficiary already exists
            if self.beneficiaries.iter().any(|b| b.account == account) {
                return Err(Error::InvalidBeneficiary);
            }
            
            let beneficiary = Beneficiary {
                account,
                share_percentage,
                pending_balance: 0,
                total_withdrawn: 0,
            };
            
            self.beneficiaries.push(beneficiary);
            self.total_shares = self.total_shares.saturating_add(share_percentage);
            
            self.env().emit_event(BeneficiaryAdded {
                beneficiary: account,
                share_percentage,
                added_by: self.env().caller(),
            });
            
            Ok(())
        }

        /// Remove a beneficiary (only owner or managers)
        #[ink(message)]
        pub fn remove_beneficiary(&mut self, account: AccountId) -> Result<()> {
            self.ensure_not_paused()?;
            self.ensure_manager_or_owner()?;
            
            let position = self.beneficiaries
                .iter()
                .position(|b| b.account == account)
                .ok_or(Error::BeneficiaryNotFound)?;
            
            let beneficiary = self.beneficiaries.remove(position);
            self.total_shares = self.total_shares.saturating_sub(beneficiary.share_percentage);
            
            // If beneficiary has pending balance, transfer it
            if beneficiary.pending_balance > 0 {
                self.env().transfer(account, beneficiary.pending_balance)
                    .map_err(|_| Error::TransferFailed)?;
            }
            
            self.env().emit_event(BeneficiaryRemoved {
                beneficiary: account,
                removed_by: self.env().caller(),
            });
            
            Ok(())
        }

        /// Grant approval for another account to withdraw on behalf of a beneficiary
        #[ink(message)]
        pub fn approve(&mut self, spender: AccountId, amount: Balance, expires_at: Option<u64>) -> Result<()> {
            self.ensure_not_paused()?;
            let caller = self.env().caller();
            
            // Ensure caller is a beneficiary
            if !self.beneficiaries.iter().any(|b| b.account == caller) {
                return Err(Error::Unauthorized);
            }
            
            let approval = Approval {
                spender,
                amount,
                expires_at,
            };
            
            self.approvals.insert((caller, spender), &approval);
            
            self.env().emit_event(ApprovalGranted {
                owner: caller,
                spender,
                amount,
                expires_at,
            });
            
            Ok(())
        }

        /// Revoke approval for a spender
        #[ink(message)]
        pub fn revoke_approval(&mut self, spender: AccountId) -> Result<()> {
            let caller = self.env().caller();
            
            self.approvals.remove((caller, spender));
            
            self.env().emit_event(ApprovalRevoked {
                owner: caller,
                spender,
            });
            
            Ok(())
        }

        /// Withdraw funds on behalf of a beneficiary (using approval)
        #[ink(message)]
        pub fn withdraw_from(&mut self, beneficiary: AccountId, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            let caller = self.env().caller();
            
            // Get and validate approval
            let approval = self.approvals.get((beneficiary, caller))
                .ok_or(Error::InsufficientAllowance)?;
            
            // Check if approval has expired
            if let Some(expires_at) = approval.expires_at {
                let current_time = self.env().block_timestamp();
                if current_time > expires_at {
                    return Err(Error::InsufficientAllowance);
                }
            }
            
            if approval.amount < amount {
                return Err(Error::InsufficientAllowance);
            }
            
            // Find beneficiary and check balance
            let beneficiary_index = self.beneficiaries
                .iter()
                .position(|b| b.account == beneficiary)
                .ok_or(Error::BeneficiaryNotFound)?;
            
            if self.beneficiaries[beneficiary_index].pending_balance < amount {
                return Err(Error::InsufficientBalance);
            }
            
            // Update beneficiary balance
            self.beneficiaries[beneficiary_index].pending_balance = 
                self.beneficiaries[beneficiary_index].pending_balance.saturating_sub(amount);
            self.beneficiaries[beneficiary_index].total_withdrawn = 
                self.beneficiaries[beneficiary_index].total_withdrawn.saturating_add(amount);
            
            // Update approval
            let mut updated_approval = approval;
            updated_approval.amount = updated_approval.amount.saturating_sub(amount);
            
            if updated_approval.amount == 0 {
                self.approvals.remove((beneficiary, caller));
            } else {
                self.approvals.insert((beneficiary, caller), &updated_approval);
            }
            
            // Transfer funds to the caller (spender)
            self.env().transfer(caller, amount)
                .map_err(|_| Error::TransferFailed)?;
            
            self.env().emit_event(WithdrawalByApproval {
                beneficiary,
                spender: caller,
                amount,
            });
            
            Ok(())
        }

        /// Withdraw own funds (beneficiary)
        #[ink(message)]
        pub fn withdraw(&mut self, amount: Balance) -> Result<()> {
            self.ensure_not_paused()?;
            let caller = self.env().caller();
            
            let beneficiary_index = self.beneficiaries
                .iter()
                .position(|b| b.account == caller)
                .ok_or(Error::Unauthorized)?;
            
            if self.beneficiaries[beneficiary_index].pending_balance < amount {
                return Err(Error::InsufficientBalance);
            }
            
            self.beneficiaries[beneficiary_index].pending_balance = 
                self.beneficiaries[beneficiary_index].pending_balance.saturating_sub(amount);
            self.beneficiaries[beneficiary_index].total_withdrawn = 
                self.beneficiaries[beneficiary_index].total_withdrawn.saturating_add(amount);
            
            self.env().transfer(caller, amount)
                .map_err(|_| Error::TransferFailed)?;
            
            Ok(())
        }

        /// Add a manager (only owner)
        #[ink(message)]
        pub fn add_manager(&mut self, manager: AccountId) -> Result<()> {
            self.ensure_owner()?;
            
            self.managers.insert(manager, &true);
            
            self.env().emit_event(ManagerAdded {
                manager,
                added_by: self.env().caller(),
            });
            
            Ok(())
        }

        /// Remove a manager (only owner)
        #[ink(message)]
        pub fn remove_manager(&mut self, manager: AccountId) -> Result<()> {
            self.ensure_owner()?;
            
            self.managers.remove(manager);
            
            self.env().emit_event(ManagerRemoved {
                manager,
                removed_by: self.env().caller(),
            });
            
            Ok(())
        }

        /// Pause the contract (only owner)
        #[ink(message)]
        pub fn pause(&mut self) -> Result<()> {
            self.ensure_owner()?;
            self.paused = true;
            
            self.env().emit_event(ContractPaused {
                by: self.env().caller(),
            });
            
            Ok(())
        }

        /// Unpause the contract (only owner)
        #[ink(message)]
        pub fn unpause(&mut self) -> Result<()> {
            self.ensure_owner()?;
            self.paused = false;
            
            self.env().emit_event(ContractUnpaused {
                by: self.env().caller(),
            });
            
            Ok(())
        }

        /// Transfer ownership (only current owner)
        #[ink(message)]
        pub fn transfer_ownership(&mut self, new_owner: AccountId) -> Result<()> {
            self.ensure_owner()?;
            self.owner = new_owner;
            Ok(())
        }

        // Query functions

        /// Get contract owner
        #[ink(message)]
        pub fn get_owner(&self) -> AccountId {
            self.owner
        }

        /// Check if account is a manager
        #[ink(message)]
        pub fn is_manager(&self, account: AccountId) -> bool {
            self.managers.get(account).unwrap_or(false)
        }

        /// Get all beneficiaries
        #[ink(message)]
        pub fn get_beneficiaries(&self) -> Vec<Beneficiary> {
            self.beneficiaries.clone()
        }

        /// Get beneficiary info
        #[ink(message)]
        pub fn get_beneficiary(&self, account: AccountId) -> Option<Beneficiary> {
            self.beneficiaries.iter().find(|b| b.account == account).cloned()
        }

        /// Get approval amount
        #[ink(message)]
        pub fn get_approval(&self, owner: AccountId, spender: AccountId) -> Balance {
            self.approvals.get((owner, spender))
                .map(|a| a.amount)
                .unwrap_or(0)
        }

        /// Get total shares allocated
        #[ink(message)]
        pub fn get_total_shares(&self) -> u8 {
            self.total_shares
        }

        /// Check if contract is paused
        #[ink(message)]
        pub fn is_paused(&self) -> bool {
            self.paused
        }

        /// Get contract statistics
        #[ink(message)]
        pub fn get_stats(&self) -> (Balance, Balance, Balance) {
            (
                self.total_received,
                self.total_distributed,
                self.env().balance()
            )
        }

        // Private helper functions

        /// Distribute funds among beneficiaries
        fn distribute_funds(&mut self, amount: Balance) -> Result<()> {
            if self.beneficiaries.is_empty() || self.total_shares == 0 {
                return Ok(());
            }

            for beneficiary in &mut self.beneficiaries {
                let share_amount = amount
                    .saturating_mul(beneficiary.share_percentage as Balance)
                    .saturating_div(100);
                
                beneficiary.pending_balance = beneficiary.pending_balance.saturating_add(share_amount);
            }
            
            self.total_distributed = self.total_distributed.saturating_add(amount);
            
            self.env().emit_event(FundsDistributed {
                total_amount: amount,
                beneficiary_count: self.beneficiaries.len() as u32,
            });
            
            Ok(())
        }

        /// Ensure caller is the owner
        fn ensure_owner(&self) -> Result<()> {
            if self.env().caller() == self.owner {
                Ok(())
            } else {
                Err(Error::Unauthorized)
            }
        }

        /// Ensure caller is owner or manager
        fn ensure_manager_or_owner(&self) -> Result<()> {
            let caller = self.env().caller();
            if caller == self.owner || self.managers.get(caller).unwrap_or(false) {
                Ok(())
            } else {
                Err(Error::Unauthorized)
            }
        }

        /// Ensure contract is not paused
        fn ensure_not_paused(&self) -> Result<()> {
            if self.paused {
                Err(Error::ContractPaused)
            } else {
                Ok(())
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[ink::test]
        fn constructor_works() {
            let contract = SplitPayment::new();
            assert_eq!(contract.is_paused(), false);
            assert_eq!(contract.get_total_shares(), 0);
        }

        #[ink::test]
        fn add_beneficiary_works() {
            let mut contract = SplitPayment::new();
            let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
            
            assert!(contract.add_beneficiary(accounts.alice, 50).is_ok());
            assert_eq!(contract.get_total_shares(), 50);
            
            let beneficiary = contract.get_beneficiary(accounts.alice).unwrap();
            assert_eq!(beneficiary.share_percentage, 50);
        }

        #[ink::test]
        fn approval_system_works() {
            let mut contract = SplitPayment::new();
            let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
            
            // Add beneficiary first
            contract.add_beneficiary(accounts.alice, 50).unwrap();
            
            // Set caller to alice for approval
            ink::env::test::set_caller::<ink::env::DefaultEnvironment>(accounts.alice);
            
            // Grant approval
            assert!(contract.approve(accounts.bob, 1000, None).is_ok());
            assert_eq!(contract.get_approval(accounts.alice, accounts.bob), 1000);
            
            // Revoke approval
            assert!(contract.revoke_approval(accounts.bob).is_ok());
            assert_eq!(contract.get_approval(accounts.alice, accounts.bob), 0);
        }

        #[ink::test]
        fn access_control_works() {
            let mut contract = SplitPayment::new();
            let accounts = ink::env::test::default_accounts::<ink::env::DefaultEnvironment>();
            
            // Non-owner cannot add manager
            ink::env::test::set_caller::<ink::env::DefaultEnvironment>(accounts.alice);
            assert_eq!(contract.add_manager(accounts.bob), Err(Error::Unauthorized));
            
            // Owner can add manager
            ink::env::test::set_caller::<ink::env::DefaultEnvironment>(accounts.eve); // Owner
            assert!(contract.add_manager(accounts.alice).is_ok());
            assert!(contract.is_manager(accounts.alice));
        }
    }
}