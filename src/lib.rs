//! A NFT smart contract example using the Concordium Token Standard CIS2.
//!
//! # Description
//! An instance of this smart contract can contain a number of different token
//! each identified by a token ID. A token is then globally identified by the
//! contract address together with the token ID.
//!
//! In this example the contract is initialized with no tokens, and tokens can
//! be minted through a `mint` contract function, which will only succeed for
//! the contract owner. No functionality to burn token is defined in this
//! example.
//!
//! Note: The word 'address' refers to either an account address or a
//! contract address.
//!
//! As follows from the CIS2 specification, the contract has a `transfer`
//! function for transferring an amount of a specific token type from one
//! address to another address. An address can enable and disable one or more
//! addresses as operators. An operator of some address is allowed to transfer
//! any tokens owned by this address.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
use alloc::vec::Vec;
use bs58;

use concordium_cis2::*;
use concordium_std::*;

/// The baseurl for the token metadata, gets appended with the token ID as hex
/// encoding before emitted in the TokenMetadata event.
const TOKEN_METADATA_BASE_URL: &str = " https://web3id.backend.aesirx.io:8001/licenses/";

/// List of supported standards by this contract address.
const SUPPORTS_STANDARDS: [StandardIdentifier<'static>; 2] =
    [CIS0_STANDARD_IDENTIFIER, CIS2_STANDARD_IDENTIFIER];

// Types

/// Contract token ID type.
/// To save bytes we use a token ID type limited to a `u32`.
type ContractTokenId = TokenIdU32;

/// Contract token amount.
/// Since the tokens are non-fungible the total supply of any token will be at
/// most 1 and it is fine to use a small type for representing token amounts.
type ContractTokenAmount = TokenAmountU8;

// Web3Id, essentially a string
type Web3Id = String;

#[derive(Debug, Serialize, Clone, SchemaType)]
pub struct TokenMetadata {
    /// The URL following the specification RFC1738.
    #[concordium(size_length = 2)]
    pub url: String,
    /// A optional hash of the content.
    #[concordium(size_length = 2)]
    pub hash: String,
}

/// The parameter for the contract function `mint` which mints a token to a given address
#[derive(Serial, Deserial, SchemaType)]
struct MintParams {
    /// Owner of the newly minted token.
    owner: AccountAddress,
    /// Token
    token: ContractTokenId,
    /// Web3Id
    web3id: Web3Id,
}

/// Parameter type for the burn function
#[derive(Serial, Deserial, SchemaType)]
struct BurnParams {
    token_id: ContractTokenId,
    owner: Address,
    amount: ContractTokenAmount,
}

/// The state for each address.
#[derive(Serial, DeserialWithState, Deletable)]
#[concordium(state_parameter = "S")]
struct AddressState<S> {
    /// The tokens owned by this address.
    owned_tokens: StateSet<ContractTokenId, S>,
    /// The address which are currently enabled as operators for this address.
    operators: StateSet<Address, S>,
}

impl<S: HasStateApi> AddressState<S> {
    fn empty(state_builder: &mut StateBuilder<S>) -> Self {
        AddressState {
            owned_tokens: state_builder.new_set(),
            operators: state_builder.new_set(),
        }
    }
}

/// The contract state.
// Note: The specification does not specify how to structure the contract state
// and this could be structured in a more space efficient way depending on the use case.
#[derive(Serial, DeserialWithState)]
#[concordium(state_parameter = "S")]
struct State<S> {
    /// The state for each address.
    state: StateMap<Address, AddressState<S>, S>,
    /// All of the token IDs
    all_tokens: StateSet<ContractTokenId, S>,
    /// Map with contract addresses providing implementations of additional
    /// standards.
    implementors: StateMap<StandardIdentifierOwned, Vec<ContractAddress>, S>,
    // Metadata
    metadata: StateMap<ContractTokenId, TokenMetadata, S>,
    // Valid global operators for minting
    operators: StateSet<Address, S>,
    /// The owner of the contract
    owner: Address,
}

/// The parameter type for the contract function `setImplementors`.
/// Takes a standard identifier and list of contract addresses providing
/// implementations of this standard.
#[derive(Debug, Serialize, SchemaType)]
struct SetImplementorsParams {
    /// The identifier for the standard.
    id: StandardIdentifierOwned,
    /// The addresses of the implementors of the standard.
    implementors: Vec<ContractAddress>,
}

/// The custom errors the contract can produce.
#[derive(Serialize, Debug, PartialEq, Eq, Reject, SchemaType)]
enum CustomContractError {
    /// Failed parsing the parameter.
    #[from(ParseError)]
    ParseParams,
    /// Failed logging: Log is full.
    LogFull,
    /// Failed logging: Log is malformed.
    LogMalformed,
    /// Failing to mint new tokens because one of the token IDs already exists
    /// in this contract.
    TokenIdAlreadyExists,
    /// Failed to invoke a contract.
    InvokeContractError,
    // Invalid Web3 ID
    InvalidWeb3Id,
    /// License not found
    LicenseNotFound,
    Unauthorized,
}

/// Wrapping the custom errors in a type with CIS2 errors.
type ContractError = Cis2Error<CustomContractError>;

type ContractResult<A> = Result<A, ContractError>;

/// Mapping the logging errors to CustomContractError.
impl From<LogError> for CustomContractError {
    fn from(le: LogError) -> Self {
        match le {
            LogError::Full => Self::LogFull,
            LogError::Malformed => Self::LogMalformed,
        }
    }
}

/// Mapping errors related to contract invocations to CustomContractError.
impl<T> From<CallContractError<T>> for CustomContractError {
    fn from(_cce: CallContractError<T>) -> Self {
        Self::InvokeContractError
    }
}

/// Mapping CustomContractError to ContractError
impl From<CustomContractError> for ContractError {
    fn from(c: CustomContractError) -> Self {
        Cis2Error::Custom(c)
    }
}

fn build_token_metadata_url(token_id: &ContractTokenId) -> String {
    // Swap the byte order of the token id to get the natural incremental number.
    let token_value = token_id.0.swap_bytes();
    // Format the number as an 8-digit decimal string with leading zeros.
    format!("{}{:08}", TOKEN_METADATA_BASE_URL, token_value)
}

// Functions for creating, updating and querying the contract state.
impl<S: HasStateApi> State<S> {
    /// Creates a new state with no tokens and a specified owner.
    fn empty(state_builder: &mut StateBuilder<S>, owner: Address) -> Self {
        State {
            state: state_builder.new_map(),
            all_tokens: state_builder.new_set(),
            implementors: state_builder.new_map(),
            metadata: state_builder.new_map(),
            operators: state_builder.new_set(),
            owner,
        }
    }

    /// Internal burn helper function. Invokes the burn functionality of the state.
/// Logs a Burn event. The function assumes that the burn is authorized.
    fn burn(
        &mut self,
        token: &ContractTokenId,
        owner: &Address,
    ) -> ContractResult<()> {
        ensure!(self.contains_token(token), ContractError::InvalidTokenId);

        if let Some(mut address_state) = self.state.get_mut(owner) {
            ensure!(
                address_state.owned_tokens.remove(token),
                ContractError::InsufficientFunds
            );
        } else {
            bail!(ContractError::InsufficientFunds)
        }

        // Remove token from all tokens
        self.all_tokens.remove(token);
        
        // Remove token metadata
        self.metadata.remove(token);

        Ok(())
    }


    /// Mint a new token with a given address as the owner
    fn mint(
        &mut self,
        token: ContractTokenId,
        metadata_url: &String,
        owner: &Address,
        state_builder: &mut StateBuilder<S>,
    ) -> ContractResult<()> {
        ensure!(
            self.all_tokens.insert(token),
            CustomContractError::TokenIdAlreadyExists.into()
        );

        let metadata_url = build_token_metadata_url(&token);
        let metadata = TokenMetadata {
            url: metadata_url,
            hash: String::from(""),
        };

        self.metadata.insert(token, metadata.clone());

        let mut owner_state = self
            .state
            .entry(*owner)
            .or_insert_with(|| AddressState::empty(state_builder));
        owner_state.owned_tokens.insert(token);
        Ok(())
    }

    /// Check that the token ID currently exists in this contract.
    #[inline(always)]
    fn contains_token(&self, token_id: &ContractTokenId) -> bool {
        self.all_tokens.contains(token_id)
    }

    /// Get the current balance of a given token ID for a given address.
    /// Results in an error if the token ID does not exist in the state.
    /// Since this contract only contains NFTs, the balance will always be
    /// either 1 or 0.
    fn balance(
        &self,
        token_id: &ContractTokenId,
        address: &Address,
    ) -> ContractResult<ContractTokenAmount> {
        ensure!(self.contains_token(token_id), ContractError::InvalidTokenId);
        let balance = self
            .state
            .get(address)
            .map(|address_state| u8::from(address_state.owned_tokens.contains(token_id)))
            .unwrap_or(0);
        Ok(balance.into())
    }

    /// Check if a given address is an operator of a given owner address.
    fn is_operator(&self, address: &Address, owner: &Address) -> bool {
        self.state
            .get(owner)
            .map(|address_state| address_state.operators.contains(address))
            .unwrap_or(false)
    }

    /// Update the state with a transfer of some token.
    /// Results in an error if the token ID does not exist in the state or if
    /// the from address have insufficient tokens to do the transfer.
    fn transfer(
        &mut self,
        token_id: &ContractTokenId,
        amount: ContractTokenAmount,
        from: &Address,
        to: &Address,
        state_builder: &mut StateBuilder<S>,
    ) -> ContractResult<()> {
        ensure!(self.contains_token(token_id), ContractError::InvalidTokenId);
        // A zero transfer does not modify the state.
        if amount == 0.into() {
            return Ok(());
        }
        // Since this contract only contains NFTs, no one will have an amount greater
        // than 1. And since the amount cannot be the zero at this point, the
        // address must have insufficient funds for any amount other than 1.
        ensure_eq!(amount, 1.into(), ContractError::InsufficientFunds);

        {
            let mut from_address_state = self
                .state
                .get_mut(from)
                .ok_or(ContractError::InsufficientFunds)?;
            // Find and remove the token from the owner, if nothing is removed, we know the
            // address did not own the token..
            let from_had_the_token = from_address_state.owned_tokens.remove(token_id);
            ensure!(from_had_the_token, ContractError::InsufficientFunds);
        }

        // Add the token to the new owner.
        let mut to_address_state = self
            .state
            .entry(*to)
            .or_insert_with(|| AddressState::empty(state_builder));
        to_address_state.owned_tokens.insert(*token_id);
        Ok(())
    }

    /// Update the state adding a new operator for minting tokens
    /// Succeeds even if the `operator` is already an operator for the
    /// `address`.
    fn add_global_operator(&mut self, operator: &Address) {
        self.operators.insert(*operator);
    }

    /// Update the state removing an operator for minting tokens
    /// Succeeds even if the `operator` is _not_ an operator for the
    /// `address`.
    fn remove_global_operator(&mut self, operator: &Address) {
        self.operators.remove(operator);
    }
    /// Update the state adding a new operator for a given address.
    /// Succeeds even if the `operator` is already an operator for the
    /// `address`.
    fn add_operator(
        &mut self,
        owner: &Address,
        operator: &Address,
        state_builder: &mut StateBuilder<S>,
    ) {
        let mut owner_state = self
            .state
            .entry(*owner)
            .or_insert_with(|| AddressState::empty(state_builder));
        owner_state.operators.insert(*operator);
    }

    /// Update the state removing an operator for a given address.
    /// Succeeds even if the `operator` is _not_ an operator for the `address`.
    fn remove_operator(&mut self, owner: &Address, operator: &Address) {
        self.state.entry(*owner).and_modify(|address_state| {
            address_state.operators.remove(operator);
        });
    }

    /// Check if state contains any implementors for a given standard.
    fn have_implementors(&self, std_id: &StandardIdentifierOwned) -> SupportResult {
        if let Some(addresses) = self.implementors.get(std_id) {
            SupportResult::SupportBy(addresses.to_vec())
        } else {
            SupportResult::NoSupport
        }
    }

    /// Set implementors for a given standard.
    fn set_implementors(
        &mut self,
        std_id: StandardIdentifierOwned,
        implementors: Vec<ContractAddress>,
    ) {
        self.implementors.insert(std_id, implementors);
    }
}

/// Build a string from TOKEN_METADATA_BASE_URL appended with the web3id
/// encoded as hex.
// fn build_token_metadata_url(web3id: &Web3Id) -> String {
//     let mut token_metadata_url = String::from(TOKEN_METADATA_BASE_URL);
//     token_metadata_url.push_str(&web3id.to_string());
//     token_metadata_url
// }

/// Function to evaluate a web3 id format
// fn check_web3id(s: &str) -> bool {
//     if s.starts_with('@') && s.len() >= 4 && s.len() <= 21 {
//         let username = &s[1..];
//         if username.chars().all(|c| c.is_alphanumeric() || c == '_') {
//             return true;
//         }
//     }
//     false
// }

// Contract functions

/// Initialize contract instance with no token types initially.
#[init(
    contract = "LicenseContract",
    event = "Cis2Event<ContractTokenId, ContractTokenAmount>"
)]
fn contract_init<S: HasStateApi>(
    ctx: &impl HasInitContext,
    state_builder: &mut StateBuilder<S>,
) -> InitResult<State<S>> {
    // Use the init_origin as the default owner
    let default_owner = ctx.init_origin();

    // Create the initial state with the deployer as the owner
    let state = State::empty(state_builder, Address::Account(default_owner));

    Ok(state)
}

#[derive(Serialize, SchemaType)]
struct ViewAddressState {
    owned_tokens: Vec<ContractTokenId>,
    operators: Vec<Address>,
}

#[derive(Serialize, SchemaType)]
struct ViewState {
    state: Vec<(Address, ViewAddressState)>,
    all_tokens: Vec<ContractTokenId>,
    operators: Vec<Address>,
}

#[receive(
    contract = "LicenseContract",
    name = "burn",
    parameter = "BurnParams",
    error = "ContractError",
    enable_logger,
    mutable
)]
fn contract_burn<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ContractResult<()> {
    // Parse the parameter.
    let BurnParams { token_id, owner, amount } = ctx.parameter_cursor().get()?;
    
    let state = host.state();

    // Get the sender who invoked this contract function.
    let sender = ctx.sender();

    // Authenticate the sender for the token burns.
    ensure!(owner == sender, ContractError::Unauthorized);

    // Burn the token
    host.state_mut().burn(&token_id, &owner)?;

    // Log the burn event with proper event emission
    logger.log(&Cis2Event::Burn(BurnEvent {
        token_id,  // Using TokenIdU32
        amount,
        owner,
    }))?;

    Ok(())
}

/// View function that returns the entire contents of the state. Meant for
/// testing.
#[receive(
    contract = "LicenseContract",
    name = "view",
    return_value = "ViewState"
)]
fn contract_view<S: HasStateApi>(
    _ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ReceiveResult<ViewState> {
    let state = host.state();

    let mut inner_state = Vec::new();
    for (k, a_state) in state.state.iter() {
        let owned_tokens = a_state.owned_tokens.iter().map(|x| *x).collect();
        let operators = a_state.operators.iter().map(|x| *x).collect();
        inner_state.push((
            *k,
            ViewAddressState {
                owned_tokens,
                operators,
            },
        ));
    }
    let all_tokens = state.all_tokens.iter().map(|x| *x).collect();
    let operators = state.operators.iter().map(|x| *x).collect();

    Ok(ViewState {
        state: inner_state,
        all_tokens,
        operators,
    })
}

/// Mint new tokens with a given address as the owner of these tokens.
/// Can only be called by the contract owner.
/// Logs a `Mint` and a `TokenMetadata` event for each token.
/// The url for the token metadata is the token ID encoded in hex, appended on
/// the `TOKEN_METADATA_BASE_URL`.
///
/// It rejects if:
/// - The sender is not the contract instance owner.
/// - Fails to parse parameter.
/// - Any of the tokens fails to be minted, which could be if:
///     - The minted token ID already exists.
///     - Fails to log Mint event
///     - Fails to log TokenMetadata event
///
/// Note: Can at most mint 32 token types in one call due to the limit on the
/// number of logs a smart contract can produce on each function call.
#[receive(
    contract = "LicenseContract",
    name = "mint",
    parameter = "MintParams",
    error = "ContractError",
    enable_logger,
    mutable
)]
fn contract_mint<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ContractResult<()> {
    // Get the contract owner
    let owner = ctx.owner();
    // Get the sender of the transaction
    let sender = ctx.sender();

    let (state, builder) = host.state_and_builder();

    if sender != state.owner && !state.operators.contains(&sender) {
        return Err(ContractError::Unauthorized); // Use the stored owner and operators for authorization
    }

    // Only the owner account and global operators can mint
    // ensure!(
    //     sender.matches_account(&owner) || state.operators.contains(&sender),
    //     ContractError::Unauthorized
    // );

    // Parse the parameter.
    let params: MintParams = ctx.parameter_cursor().get()?;

    let token_id = params.token;
    let web3id = params.web3id;
    // let token_be = u32::from_be_bytes(token_id.to_le_bytes());

    // ensure!(
    //     // check_web3id(&web3id),
    //     CustomContractError::InvalidWeb3Id.into()
    // );

    // let metadata_url = build_token_metadata_url(&web3id);
    let metadata_url = build_token_metadata_url(&token_id);

    let token_owner: Address = Address::Account(params.owner);

    // Mint the token in the state.
    state.mint(token_id, &metadata_url, &token_owner, builder)?;

    // Event for minted NFT.
    logger.log(&Cis2Event::Mint(MintEvent {
        token_id,
        amount: ContractTokenAmount::from(1),
        owner: token_owner,
    }))?;

    // Metadata URL for the NFT.
    logger.log(&Cis2Event::TokenMetadata::<_, ContractTokenAmount>(
        TokenMetadataEvent {
            token_id,
            metadata_url: MetadataUrl {
                url: metadata_url,
                hash: None,
            },
        },
    ))?;
    Ok(())
}

type TransferParameter = TransferParams<ContractTokenId, ContractTokenAmount>;

/// Execute a list of token transfers, in the order of the list.
///
/// Logs a `Transfer` event and invokes a receive hook function for every
/// transfer in the list.
///
/// It rejects if:
/// - It fails to parse the parameter.
/// - Any of the transfers fail to be executed, which could be if:
///     - The `token_id` does not exist.
///     - The sender is not the owner of the token, or an operator for this
///       specific `token_id` and `from` address.
///     - The token is not owned by the `from`.
/// - Fails to log event.
/// - Any of the receive hook function calls rejects.
#[receive(
    contract = "LicenseContract",
    name = "transfer",
    parameter = "TransferParameter",
    error = "ContractError",
    enable_logger,
    mutable
)]
fn contract_transfer<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ContractResult<()> {
    // Parse the parameter.
    let TransferParams(transfers): TransferParameter = ctx.parameter_cursor().get()?;
    // Get the sender who invoked this contract function.
    let sender = ctx.sender();

    for Transfer {
        token_id,
        amount,
        from,
        to,
        data,
    } in transfers
    {
        let (state, builder) = host.state_and_builder();
        
        // Authenticate the sender for this transfer
        // ensure!(from == sender, ContractError::Unauthorized);

        if from != state.owner  {
            return Err(ContractError::Unauthorized); // Use the stored owner and operators for authorization
        }

        let to_address = to.address();
        
        // Update the contract state
        state.transfer(&token_id, amount, &from, &to_address, builder)?;

        // Log transfer event
        logger.log(&Cis2Event::Transfer(TransferEvent {
            token_id,
            amount,
            from,
            to: to_address,
        }))?;

        // If the receiver is a contract: invoke the receive hook function.
        if let Receiver::Contract(address, function) = to {
            let parameter = OnReceivingCis2Params {
                token_id,
                amount,
                from,
                data,
            };
            host.invoke_contract(
                &address,
                &parameter,
                function.as_entrypoint_name(),
                Amount::zero(),
            )?;
        }
    }
    Ok(())
}

/// Enable or disable addresses as operators of the sender address.
/// Logs an `UpdateOperator` event.
///
/// It rejects if:
/// - It fails to parse the parameter.
/// - Fails to log event.
#[receive(
    contract = "LicenseContract",
    name = "updateOperator",
    parameter = "UpdateOperatorParams",
    error = "ContractError",
    enable_logger,
    mutable
)]
fn contract_update_operator<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ContractResult<()> {
    // Parse the parameter.
    let UpdateOperatorParams(params) = ctx.parameter_cursor().get()?;
    // Get the sender who invoked this contract function.
    let sender = ctx.sender();
    let (state, builder) = host.state_and_builder();
    for param in params {
        // Update the operator in the state.
        match param.update {
            OperatorUpdate::Add => state.add_operator(&sender, &param.operator, builder),
            OperatorUpdate::Remove => state.remove_operator(&sender, &param.operator),
        }

        // Log the appropriate event
        logger.log(
            &Cis2Event::<ContractTokenId, ContractTokenAmount>::UpdateOperator(
                UpdateOperatorEvent {
                    owner: sender,
                    operator: param.operator,
                    update: param.update,
                },
            ),
        )?;
    }

    Ok(())
}

/// Takes a list of queries. Each query is an owner address and some address to
/// check as an operator of the owner address.
///
/// It rejects if:
/// - It fails to parse the parameter.
#[receive(
    contract = "LicenseContract",
    name = "operatorOf",
    parameter = "OperatorOfQueryParams",
    return_value = "OperatorOfQueryResponse",
    error = "ContractError"
)]
fn contract_operator_of<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<OperatorOfQueryResponse> {
    // Parse the parameter.
    let params: OperatorOfQueryParams = ctx.parameter_cursor().get()?;
    // Build the response.
    let mut response = Vec::with_capacity(params.queries.len());
    for query in params.queries {
        // Query the state for address being an operator of owner.
        let is_operator = host.state().is_operator(&query.address, &query.owner);
        response.push(is_operator);
    }
    let result = OperatorOfQueryResponse::from(response);
    Ok(result)
}

/// Parameter type for the CIS-2 function `balanceOf` specialized to the subset
/// of TokenIDs used by this contract.
type ContractBalanceOfQueryParams = BalanceOfQueryParams<ContractTokenId>;
/// Response type for the CIS-2 function `balanceOf` specialized to the subset
/// of TokenAmounts used by this contract.
type ContractBalanceOfQueryResponse = BalanceOfQueryResponse<ContractTokenAmount>;

/// Get the balance of given token IDs and addresses.
///
/// It rejects if:
/// - It fails to parse the parameter.
/// - Any of the queried `token_id` does not exist.
#[receive(
    contract = "LicenseContract",
    name = "balanceOf",
    parameter = "ContractBalanceOfQueryParams",
    return_value = "ContractBalanceOfQueryResponse",
    error = "ContractError"
)]
fn contract_balance_of<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<ContractBalanceOfQueryResponse> {
    // Parse the parameter.
    let params: ContractBalanceOfQueryParams = ctx.parameter_cursor().get()?;
    // Build the response.
    let mut response = Vec::with_capacity(params.queries.len());
    for query in params.queries {
        // Query the state for balance.
        let amount = host.state().balance(&query.token_id, &query.address)?;
        response.push(amount);
    }
    let result = ContractBalanceOfQueryResponse::from(response);
    Ok(result)
}

/// Parameter type for the CIS-2 function `tokenMetadata` specialized to the
/// subset of TokenIDs used by this contract.
type ContractTokenMetadataQueryParams = TokenMetadataQueryParams<ContractTokenId>;

/// Get the token metadata URLs and checksums given a list of token IDs.
///
/// It rejects if:
/// - It fails to parse the parameter.
/// - Any of the queried `token_id` does not exist.
#[receive(
    contract = "LicenseContract",
    name = "tokenMetadata",
    parameter = "ContractTokenMetadataQueryParams",
    return_value = "TokenMetadataQueryResponse",
    error = "ContractError"
)]
fn contract_token_metadata<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<TokenMetadataQueryResponse> {
    // Parse the parameter.
    let params: ContractTokenMetadataQueryParams = ctx.parameter_cursor().get()?;
    // Build the response.
    let mut response = Vec::with_capacity(params.queries.len());
    for token_id in params.queries {
        // Check the token exists.
        ensure!(
            host.state().contains_token(&token_id),
            ContractError::InvalidTokenId
        );

        let metadata_url: MetadataUrl = host
            .state()
            .metadata
            .get(&token_id)
            .map(|metadata| MetadataUrl {
                hash: None,
                url: metadata.url.to_owned(),
            })
            .ok_or(ContractError::InvalidTokenId)?;
        response.push(metadata_url);
    }
    let result = TokenMetadataQueryResponse::from(response);
    Ok(result)
}

/// Get the supported standards or addresses for a implementation given list of
/// standard identifiers.
///
/// It rejects if:
/// - It fails to parse the parameter.
#[receive(
    contract = "LicenseContract",
    name = "supports",
    parameter = "SupportsQueryParams",
    return_value = "SupportsQueryResponse",
    error = "ContractError"
)]
fn contract_supports<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<SupportsQueryResponse> {
    // Parse the parameter.
    let params: SupportsQueryParams = ctx.parameter_cursor().get()?;

    // Build the response.
    let mut response = Vec::with_capacity(params.queries.len());
    for std_id in params.queries {
        if SUPPORTS_STANDARDS.contains(&std_id.as_standard_identifier()) {
            response.push(SupportResult::Support);
        } else {
            response.push(host.state().have_implementors(&std_id));
        }
    }
    let result = SupportsQueryResponse::from(response);
    Ok(result)
}

/// Set the addresses for an implementation given a standard identifier and a
/// list of contract addresses.
///
/// It rejects if:
/// - Sender is not the owner of the contract instance.
/// - It fails to parse the parameter.
#[receive(
    contract = "LicenseContract",
    name = "setImplementors",
    parameter = "SetImplementorsParams",
    error = "ContractError",
    mutable
)]
fn contract_set_implementor<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<()> {
    // Authorize the sender.
    // ensure!(
    //     ctx.sender().matches_account(&ctx.owner()),
    //     ContractError::Unauthorized
    // );
    let sender = ctx.sender();

    if ctx.sender().matches_account(&ctx.owner()) {
        return Err(ContractError::Unauthorized); // Use the stored owner and operators for authorization
    }
    // Parse the parameter.
    let params: SetImplementorsParams = ctx.parameter_cursor().get()?;
    // Update the implementors in the state
    host.state_mut()
        .set_implementors(params.id, params.implementors);
    Ok(())
}

/// The parameter type for the contract function `upgrade`.
/// Takes the new module and optionally a migration function to call in the new
/// module after the upgrade.
#[derive(Serialize, SchemaType)]
struct UpgradeParams {
    /// The new module reference.
    module:  ModuleReference,
    /// Optional entrypoint to call in the new module after upgrade.
    migrate: Option<(OwnedEntrypointName, OwnedParameter)>,
}

#[receive(
    contract = "LicenseContract",
    name = "upgrade",
    parameter = "UpgradeParams",
    low_level
)]
fn contract_upgrade(
    ctx: &ReceiveContext,
    host: &mut LowLevelHost,
) -> ReceiveResult<()> {
    // Check that only the owner is authorized to upgrade the smart contract.
    // ensure!(ctx.sender().matches_account(&ctx.owner()));
    let sender = ctx.sender();

    if !sender.matches_account(&ctx.owner()) {
        // Optionally log a message or handle unauthorized access
        return Ok(()); // Exit the function without performing the upgrade
    }
    // Parse the parameter.
    let params: UpgradeParams = ctx.parameter_cursor().get()?;
    // Trigger the upgrade.
    host.upgrade(params.module)?;
    // Call the migration function if provided.
    if let Some((func, parameters)) = params.migrate {
        host.invoke_contract_raw(
            &ctx.self_address(),
            parameters.as_parameter(),
            func.as_entrypoint_name(),
            Amount::zero(),
        )?;
    }
    Ok(())
}

// Function to update the owner
fn update_owner<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    state: &mut State<S>,
    new_owner_address: &str,
) -> Result<(), CustomContractError> {
    // Check if the caller is the current owner
    let caller = ctx.sender();
    if caller != state.owner {
        return Err(CustomContractError::Unauthorized);
    }

    let new_owner_address = "4MwARWeXdMs3YZ5MPPn2561ceani6AJAVTNPtwS6tceaG2qatK";
    // Decode the new owner address from Base58
    let new_owner_bytes = bs58::decode(new_owner_address)
        .into_vec()
        .map_err(|_| CustomContractError::ParseParams)?; // Handle parsing errors

    // Ensure the byte array is exactly 32 bytes
    let new_owner = AccountAddress(new_owner_bytes.try_into().map_err(|_| CustomContractError::ParseParams)?);

    // Update the owner in the state
    state.owner = Address::Account(new_owner);

    Ok(())
}