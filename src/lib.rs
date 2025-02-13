use concordium_cis2::*;
use concordium_std::*;

// === Contract Types ===

#[derive(Debug, PartialEq, Eq, Serial, Deserial, SchemaType)]
enum ContractError {
    LicenseAlreadyExists,
    LicenseNotFound,
    Unauthorized,
    ParseError,
    InvalidStandardIdentifier,
    InvalidTokenId,
    InvalidExpiry
}

impl From<ContractError> for Reject {
    fn from(error: ContractError) -> Self {
        match error {
            ContractError::LicenseAlreadyExists => Reject::new(1).expect("Failed to create Reject"),
            ContractError::LicenseNotFound => Reject::new(2).expect("Failed to create Reject"),
            ContractError::Unauthorized => Reject::new(3).expect("Failed to create Reject"),
            ContractError::ParseError => Reject::new(4).expect("Failed to create Reject"),
            ContractError::InvalidTokenId => Reject::new(5).expect("Failed to create Reject"),
            ContractError::InvalidStandardIdentifier => Reject::new(6).expect("Failed to create Reject"),
            ContractError::InvalidExpiry => Reject::new(7).expect("Failed to create Reject"),
        }
    }
}

#[derive(Debug, Serial, Deserial, Clone, SchemaType)]
enum LicenseType {
    Monthly,
    Yearly,
    OneTime,
}

#[derive(Debug, Serial, Deserial, Clone, SchemaType)]
enum RenewalStatus {
    Active,
    Inactive,
}

#[derive(Debug, Serial, Deserial, Clone, SchemaType)]
struct ValidityPeriod {
    valid_from: Timestamp,
    valid_until: Timestamp,
}

#[derive(Debug, Serial, Deserial, Clone, SchemaType, PartialEq)]
enum LicenseState {
    Active,
    Paused,
    Dormant,
    Expired,
    Suspended,
}

#[derive(Debug, Serial, Deserial, Clone, SchemaType)]
struct Transaction {
    from: Address,
    to: Address,
    date: Timestamp,
}

#[derive(Debug, Serial, Deserial, SchemaType, Clone)]
struct LicenseMetadata {
    license_type: LicenseType,
    validity_period: ValidityPeriod,
    license_state: LicenseState,
    associated_domains: Vec<String>,
    application_id: Option<String>,
    minting_date: Timestamp,
    minted_by: Address,
    token_id: TokenIdU8,
    token_name: String,
    description: String,
    image_url: String,
    metadata_hash: Option<[u8; 32]>,
    previous_owner: Option<Address>,
    current_owner: Address,
    transaction_history: Vec<Transaction>,
    payment_method: String,
    payment_status: String,
    price_paid: String,
    expiration_date: Timestamp,
    renewal_status: RenewalStatus,
}

#[derive(Serial, DeserialWithState)]
#[concordium(state_parameter = "S")]
struct State<S: HasStateApi> {
    licenses: StateMap<TokenIdU8, LicenseMetadata, S>,
    operators: StateMap<Address, Address, S>,
    balances: StateMap<Address, u64, S>,
}

// === Contract Init Function ===

#[init(contract = "LicenseContract")]
fn contract_init<S: HasStateApi>(_ctx: &InitContext, state_builder: &mut StateBuilder<S>) -> InitResult<State<S>> {
    Ok(State {
        licenses: state_builder.new_map(),
        operators: state_builder.new_map(),
        balances: state_builder.new_map(),
    })
}

// === Contract Receive Functions ===

#[receive(
    contract = "LicenseContract",
    name = "supports",
    parameter = "SupportsQueryParams",
    return_value = "SupportsQueryResponse"
)]
fn supports<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ReceiveResult<SupportsQueryResponse> {
    let query: SupportsQueryParams = ctx.parameter_cursor().get()?;
    let cis2_identifier = StandardIdentifier::new("CIS2").map_err(|_| ContractError::InvalidStandardIdentifier)?;
    let supported: Vec<SupportResult> = query.queries.iter()
        .map(|id| {
            if id.as_standard_identifier() == cis2_identifier {
                SupportResult::Support
            } else {
                SupportResult::NoSupport
            }
        })
        .collect();
    Ok(SupportsQueryResponse { results: supported })
}

#[receive(
    contract = "LicenseContract",
    name = "mint",
    payable,
    parameter = "LicenseMetadata",
    enable_logger,
    mutable
)]
fn mint<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    _amount: Amount,
    logger: &mut impl HasLogger,
) -> ReceiveResult<()> {
    let state = host.state_mut();
    let metadata: LicenseMetadata = ctx.parameter_cursor().get()?;
    let token_id = metadata.token_id;

    if state.licenses.get(&token_id).is_some() {
        return Err(ContractError::LicenseAlreadyExists.into());
    }

    let sender = ctx.sender();
    let current_time = ctx.metadata().slot_time();

    let mut metadata = metadata;
    metadata.minted_by = sender;
    metadata.minting_date = current_time;
    metadata.current_owner = sender;

    state.licenses.insert(token_id, metadata.clone());
    state.balances.entry(sender).and_modify(|e| *e += 1).or_insert(1);

    // Emit mint event
    logger.log(&Cis2Event::Mint(MintEvent {
        token_id,
        amount: TokenAmountU8(1),
        owner: sender,
    }))?;

    Ok(())
}

#[receive(
    contract = "LicenseContract",
    name = "transfer",
    parameter = "(TokenIdU8, Address)",
    enable_logger,
    mutable
)]
fn transfer<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ReceiveResult<()> {
    let state = host.state_mut();
    let (token_id, new_owner): (TokenIdU8, Address) = ctx.parameter_cursor().get()?;
    let sender = ctx.sender();
    let current_time = ctx.metadata().slot_time();

    // Get and verify license ownership
    let owner = {
        let license = match state.licenses.get(&token_id) {
            Some(license) => license,
            None => return Err(ContractError::LicenseNotFound.into()),
        };

        // Verify ownership or operator status
        if sender != license.current_owner {
            return Err(ContractError::Unauthorized.into());
        }

        license.current_owner
    };

    // Update license ownership
    if let Some(mut license) = state.licenses.get_mut(&token_id) {
        license.current_owner = new_owner;
    }

    // Update balances
    if let Some(mut from_balance) = state.balances.get_mut(&owner) {
        *from_balance = from_balance.saturating_sub(1);
    }
    state.balances.entry(new_owner).and_modify(|e| *e += 1).or_insert(1);

    // Emit transfer event
    logger.log(&Cis2Event::<TokenIdU8, TokenAmountU8>::Transfer(TransferEvent {
        token_id,
        amount: TokenAmountU8(1),
        from: sender,
        to: new_owner,
    }))?;

    Ok(())
}


#[receive(
    contract = "LicenseContract",
    name = "burn",
    parameter = "TokenIdU8",
    enable_logger,
    mutable
)]
fn burn<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ReceiveResult<()> {
    let state = host.state_mut();
    let token_id: TokenIdU8 = ctx.parameter_cursor().get()?;
    let sender = ctx.sender();

    // Get license details and verify ownership
    let owner = {
        let license = match state.licenses.get(&token_id) {
            Some(license) => license,
            None => return Err(ContractError::LicenseNotFound.into()),
        };

        // Verify ownership or operator status
        if sender != license.current_owner && state.operators.get(&sender).is_none() {
            return Err(ContractError::Unauthorized.into());
        }

        license.current_owner
    }; // License reference is dropped here

    // Now we can modify state
    state.licenses.remove(&token_id);
    
    // Update balance for the owner - fixed mutability
    if let Some(mut balance) = state.balances.get_mut(&owner) {
        *balance = balance.saturating_sub(1);
    }

    // Emit burn event
    logger.log(&Cis2Event::<TokenIdU8, TokenAmountU8>::Burn(BurnEvent {
        token_id,
        amount: TokenAmountU8(1),
        owner: sender,
    }))?;

    Ok(())
}

#[receive(
    contract = "LicenseContract",
    name = "suspend",
    parameter = "TokenIdU8",
    enable_logger,
    mutable
)]
fn suspend<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ReceiveResult<()> {
    let state = host.state_mut();
    let token_id: TokenIdU8 = ctx.parameter_cursor().get()?;

    // Verify admin rights
    if !ctx.sender().matches_account(&ctx.owner()) {
        return Err(ContractError::Unauthorized.into());
    }

    // Get and update license state - fixed error handling
    let mut license = match state.licenses.get_mut(&token_id) {
        Some(license) => license,
        None => return Err(ContractError::InvalidTokenId.into()),
    };
    license.license_state = LicenseState::Suspended;

    // Emit metadata update event
    logger.log(&Cis2Event::<TokenIdU8, TokenAmountU8>::TokenMetadata(
        TokenMetadataEvent {
            token_id,
            metadata_url: MetadataUrl {
                url: format!("token-{}", token_id),
                hash: None,
            },
        }
    ))?;

    Ok(())
}

#[receive(
    contract = "LicenseContract",
    name = "reactivate",
    parameter = "TokenIdU8",
    enable_logger,
    mutable
)]
fn reactivate<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ReceiveResult<()> {
    let state = host.state_mut();
    let token_id: TokenIdU8 = ctx.parameter_cursor().get()?;
    let sender = ctx.sender();

    // Verify admin rights
    if !ctx.sender().matches_account(&ctx.owner()) {
        return Err(ContractError::Unauthorized.into());
    }

    // Get and update license state - using match instead of ok_or
    let mut license = match state.licenses.get_mut(&token_id) {
        Some(license) => license,
        None => return Err(ContractError::InvalidTokenId.into()),
    };
    license.license_state = LicenseState::Active;

    // Emit metadata update event
    logger.log(&Cis2Event::<TokenIdU8, TokenAmountU8>::TokenMetadata(
        TokenMetadataEvent {
            token_id,
            metadata_url: MetadataUrl {
                url: format!("token-{}", token_id),
                hash: None,
            },
        }
    ))?;

    Ok(())
}

#[derive(Serial, Deserial, SchemaType)]
struct OperatorUpdate {
    token_id: TokenIdU8,
    operator: Address,
    update: bool,  // true to add, false to remove
}

#[receive(
    contract = "LicenseContract",
    name = "updateOperator",
    parameter = "OperatorUpdate",
    enable_logger,
    mutable
)]
fn update_operator<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    logger: &mut impl HasLogger,
) -> ReceiveResult<()> {
    let state = host.state_mut();
    let params: OperatorUpdate = ctx.parameter_cursor().get()?;
    
    // Get the license and verify ownership
    let license = match state.licenses.get(&params.token_id) {
        Some(license) => license,
        None => return Err(ContractError::LicenseNotFound.into()),
    };

    // Ensure only the owner can update operators
    if ctx.sender() != license.current_owner {
        return Err(ContractError::Unauthorized.into());
    }

    // Update operator status
    if params.update {
        state.operators.insert(params.operator, license.current_owner);
    } else {
        state.operators.remove(&params.operator);
    }

    // Fixed event emission using CIS2's UpdateOperator type
    logger.log(&Cis2Event::<TokenIdU8, TokenAmountU8>::UpdateOperator(
        UpdateOperatorEvent {
            owner: license.current_owner,
            operator: params.operator,
            update: if params.update { 
                concordium_cis2::OperatorUpdate::Add 
            } else { 
                concordium_cis2::OperatorUpdate::Remove 
            },
        }
    ))?;

    Ok(())
}

// Define the view struct for license details
#[derive(Serialize, SchemaType)]
struct LicenseView {
    token_id: TokenIdU8,
    current_owner: Address,
    license_state: LicenseState,
    minting_date: Timestamp,
    minted_by: Address,
}

// View function to get a single license's details
#[receive(
    contract = "LicenseContract",
    name = "viewLicense",
    parameter = "TokenIdU8",
    return_value = "LicenseView"
)]
fn view_license<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ReceiveResult<LicenseView> {
    let state = host.state();
    let token_id: TokenIdU8 = ctx.parameter_cursor().get()?;

    let license = match state.licenses.get(&token_id) {
        Some(license) => license,
        None => return Err(ContractError::LicenseNotFound.into()),
    };

    Ok(LicenseView {
        token_id,
        current_owner: license.current_owner,
        license_state: license.license_state.clone(),
        minting_date: license.minting_date,
        minted_by: license.minted_by,
    })
}

// View function to get all licenses owned by an address
#[receive(
    contract = "LicenseContract",
    name = "viewLicensesByOwner",
    parameter = "Address",
    return_value = "Vec<LicenseView>"
)]
fn view_licenses_by_owner<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &impl HasHost<State<S>, StateApiType = S>,
) -> ReceiveResult<Vec<LicenseView>> {
    let state = host.state();
    let owner: Address = ctx.parameter_cursor().get()?;
    
    let mut licenses = Vec::new();
    
    // Iterate through all licenses and collect those owned by the specified address
    for (token_id, license) in state.licenses.iter() {
        if license.current_owner == owner {
            licenses.push(LicenseView {
                token_id: *token_id,
                current_owner: license.current_owner,
                license_state: license.license_state.clone(),
                minting_date: license.minting_date,
                minted_by: license.minted_by,
            });
        }
    }

    Ok(licenses)
}

#[derive(Serial, Deserial, SchemaType)]
struct RenewalParams {
    token_id: TokenIdU8,
    new_expiry: Timestamp,
}

#[receive(
    contract = "LicenseContract",
    name = "renewLicense",
    parameter = "RenewalParams",
    payable,
    enable_logger,
    mutable
)]
fn renew_license<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    amount: Amount,
    logger: &mut impl HasLogger,
) -> ReceiveResult<()> {
    let state = host.state_mut();
    let params: RenewalParams = ctx.parameter_cursor().get()?;
    let current_time = ctx.metadata().slot_time();
    
    // Get and verify license status
    let mut license = match state.licenses.get_mut(&params.token_id) {
        Some(license) => license,
        None => return Err(ContractError::LicenseNotFound.into()),
    };

    // Verify ownership or operator status
    let sender = ctx.sender();
    if sender != license.current_owner && state.operators.get(&sender).is_none() {
        return Err(ContractError::Unauthorized.into());
    }

    // Check if license is eligible for renewal
    if license.license_state != LicenseState::Active {
        return Err(ContractError::Unauthorized.into());
    }

    // Verify new expiry is in the future
    if params.new_expiry <= current_time {
        return Err(ContractError::InvalidExpiry.into());
    }

    // Update license validity with proper ValidityPeriod type
    license.validity_period = ValidityPeriod {
        valid_from: current_time,
        valid_until: params.new_expiry,
    };

    // Emit metadata update event
    logger.log(&Cis2Event::<TokenIdU8, TokenAmountU8>::TokenMetadata(
        TokenMetadataEvent {
            token_id: params.token_id,
            metadata_url: MetadataUrl {
                url: format!("token-{}", params.token_id),
                hash: None,
            },
        }
    ))?;

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
    ensure!(ctx.sender().matches_account(&ctx.owner()));
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