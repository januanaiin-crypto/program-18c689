#![no_std]
//! Judgement Day Protocol: policy-bound Hot Zone accounts for AI agents.
//! The defender may freeze and evacuate along the owner-preset route; it can never withdraw or redirect funds.
//! Reserves (Citadel / Last Fortress) live in an external Winternitz vault and are never touched here.

use pinocchio::cpi::{Seed, Signer};
use pinocchio::error::ProgramError;
use pinocchio::sysvars::{clock::Clock, rent::Rent, Sysvar};
use pinocchio::{AccountView, Address, ProgramResult};
use pinocchio_system::instructions::{Allocate, Assign, Transfer};

#[cfg(not(feature = "no-entrypoint"))]
pinocchio::program_entrypoint!(process_instruction, 8);
pinocchio::no_allocator!();
pinocchio::nostd_panic_handler!();

pub const ID: Address = Address::from_str_const("5niYYBAr9YzrFDJXzwyUJyUqvRFHEW8cTRyiT7Kgs4pi");
const SYSTEM: Address = pinocchio_system::ID;
const LOADER: Address = Address::from_str_const("BPFLoaderUpgradeab1e11111111111111111111111");
const ZERO: Address = Address::new_from_array([0; 32]);
const CONFIG_LEN: usize = 104;
const PROFILE_LEN: usize = 520;
const CONFIG_DISC: [u8; 8] = *b"JDAYCFG1";
const PROFILE_DISC: [u8; 8] = *b"JDAYAGT1";
const MAX_RECIPIENTS: usize = 8;
const JD: u8 = 2;

mod c {
    pub const ADMIN: usize = 8;
    pub const MESH: usize = 40;
    pub const PAUSED: usize = 72;
    pub const CLOSING: usize = 73;
    pub const LEVEL: usize = 74;
    pub const BUMP: usize = 75;
    pub const AGENTS: usize = 76;
    pub const LEVEL_SINCE: usize = 84;
    pub const JD_EPOCH: usize = 92; // +1 every time the mesh level is set to JUDGEMENT DAY
    pub const REGISTRATIONS: usize = 100; // u32, +1 per register_agent (never reset)
}
mod p {
    pub const OWNER: usize = 8;
    pub const AGENT: usize = 40;
    pub const DEFENDER: usize = 72;
    pub const ROUTE: usize = 104;
    pub const ID: usize = 136;
    pub const MODE: usize = 168;
    pub const FOLLOW: usize = 169;
    pub const BUMP: usize = 170;
    pub const HOT_BUMP: usize = 171;
    pub const N: usize = 172;
    pub const RECIPIENTS: usize = 176;
    pub const CAP_TX: usize = 432; // watch tx, watch window, alert tx, alert window
    pub const WINDOW: usize = 464;
    pub const WINDOW_START: usize = 472;
    pub const SPENT: usize = 480;
    pub const EXPIRES: usize = 488;
    pub const NONCE: usize = 496;
    pub const INCIDENTS: usize = 504;
    pub const ACK_EPOCH: usize = 512; // mesh JD epoch the owner last acknowledged (Zero Trust Reboot)
}
// policy block in register_agent / update_policy args
mod a {
    pub const AGENT: usize = 0;
    pub const DEFENDER: usize = 32;
    pub const ROUTE: usize = 64;
    pub const FOLLOW: usize = 96;
    pub const CAPS: usize = 97;
    pub const WINDOW: usize = 129;
    pub const EXPIRES: usize = 137;
    pub const N: usize = 145;
    pub const RECIPIENTS: usize = 146;
}

enum Error {
    Unauthorized = 6000,
    Paused,
    BadParams,
    MathOverflow,
    Lockdown,
    Expired,
    BadNonce,
    BadRecipient,
    OverLimit,
    InsufficientFunds,
    BadAccount,
    NotLockdown,
    WindingDown,
    AgentsOpen,
    OnlyRaise,
}
fn err(e: Error) -> ProgramError {
    ProgramError::Custom(e as u32)
}
fn need(ok: bool, e: Error) -> ProgramResult {
    if ok {
        Ok(())
    } else {
        Err(err(e))
    }
}
fn rd<const N: usize>(d: &[u8], o: usize) -> Result<[u8; N], ProgramError> {
    let end = o.checked_add(N).ok_or(ProgramError::InvalidAccountData)?;
    d.get(o..end)
        .ok_or(ProgramError::InvalidAccountData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidAccountData)
}
fn byte(d: &[u8], o: usize) -> Result<u8, ProgramError> {
    d.get(o).copied().ok_or(ProgramError::InvalidAccountData)
}
fn key(d: &[u8], o: usize) -> Result<Address, ProgramError> {
    Ok(Address::new_from_array(rd(d, o)?))
}
fn u64_at(d: &[u8], o: usize) -> Result<u64, ProgramError> {
    Ok(u64::from_le_bytes(rd(d, o)?))
}
fn i64_at(d: &[u8], o: usize) -> Result<i64, ProgramError> {
    Ok(i64::from_le_bytes(rd(d, o)?))
}
fn put(d: &mut [u8], o: usize, v: &[u8]) -> ProgramResult {
    let end = o
        .checked_add(v.len())
        .ok_or(ProgramError::InvalidAccountData)?;
    d.get_mut(o..end)
        .ok_or(ProgramError::InvalidAccountData)?
        .copy_from_slice(v);
    Ok(())
}
fn add(a: u64, b: u64) -> Result<u64, ProgramError> {
    a.checked_add(b).ok_or(err(Error::MathOverflow))
}
fn sub(a: u64, b: u64) -> Result<u64, ProgramError> {
    a.checked_sub(b).ok_or(err(Error::MathOverflow))
}
fn account(a: &[AccountView], i: usize) -> Result<&AccountView, ProgramError> {
    a.get(i).ok_or(ProgramError::NotEnoughAccountKeys)
}
fn writable(a: &AccountView) -> ProgramResult {
    need(a.is_writable(), Error::Unauthorized)
}
fn signer(a: &AccountView) -> ProgramResult {
    if a.is_signer() {
        Ok(())
    } else {
        Err(ProgramError::MissingRequiredSignature)
    }
}
fn system(a: &AccountView) -> ProgramResult {
    need(a.address() == &SYSTEM && a.executable(), Error::BadAccount)
}
fn find(seeds: &[&[u8]], a: &AccountView) -> Result<u8, ProgramError> {
    let (address, bump) = Address::find_program_address(seeds, &ID);
    need(a.address() == &address, Error::BadAccount)?;
    Ok(bump)
}
fn derived(seeds: &[&[u8]]) -> Result<Address, ProgramError> {
    Address::create_program_address(seeds, &ID).map_err(|_| err(Error::BadAccount))
}
fn check_pda(a: &AccountView, seeds: &[&[u8]]) -> ProgramResult {
    need(a.address() == &derived(seeds)?, Error::BadAccount)
}
// System-owned, data-less PDA that only holds lamports.
fn money(a: &AccountView, seeds: &[&[u8]]) -> ProgramResult {
    check_pda(a, seeds)?;
    need(a.owned_by(&SYSTEM) && a.data_len() == 0, Error::BadAccount)
}
fn load<const N: usize>(a: &AccountView, disc: &[u8; 8]) -> Result<[u8; N], ProgramError> {
    if !a.owned_by(&ID) {
        return Err(ProgramError::IllegalOwner);
    }
    if a.data_len() != N {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = a.try_borrow()?;
    if rd::<8>(&data, 0)? != *disc {
        return Err(ProgramError::InvalidAccountData);
    }
    rd(&data, 0)
}
fn config(a: &AccountView) -> Result<[u8; CONFIG_LEN], ProgramError> {
    let d = load(a, &CONFIG_DISC)?;
    check_pda(a, &[b"config", &[byte(&d, c::BUMP)?]])?;
    Ok(d)
}
fn profile(a: &AccountView) -> Result<[u8; PROFILE_LEN], ProgramError> {
    let d = load(a, &PROFILE_DISC)?;
    check_pda(
        a,
        &[
            b"agent",
            &rd::<32>(&d, p::OWNER)?,
            &rd::<32>(&d, p::ID)?,
            &[byte(&d, p::BUMP)?],
        ],
    )?;
    Ok(d)
}
fn hot(a: &AccountView, prof: &AccountView, d: &[u8]) -> ProgramResult {
    money(a, &[b"hot", prof.address().as_ref(), &[byte(d, p::HOT_BUMP)?]])
}
fn save(a: &mut AccountView, d: &[u8]) -> ProgramResult {
    writable(a)?;
    let mut data = a.try_borrow_mut()?;
    if data.len() != d.len() {
        return Err(ProgramError::InvalidAccountData);
    }
    data.copy_from_slice(d);
    Ok(())
}
fn save_at(a: &mut [AccountView], i: usize, d: &[u8]) -> ProgramResult {
    save(a.get_mut(i).ok_or(ProgramError::NotEnoughAccountKeys)?, d)
}
fn auth(d: &[u8], offset: usize, a: &AccountView) -> ProgramResult {
    signer(a)?;
    need(a.address() == &key(d, offset)?, Error::Unauthorized)
}
fn now() -> Result<i64, ProgramError> {
    Ok(Clock::get()?.unix_timestamp)
}
fn rent(len: usize) -> Result<u64, ProgramError> {
    Rent::get()?.try_minimum_balance(len)
}
fn transfer(from: &AccountView, to: &AccountView, lamports: u64, seeds: &[Seed]) -> ProgramResult {
    writable(from)?;
    writable(to)?;
    if lamports == 0 {
        return Ok(());
    }
    let ix = Transfer { from, to, lamports };
    if seeds.is_empty() {
        ix.invoke()
    } else {
        ix.invoke_signed(&[Signer::from(seeds)])
    }
}
// Pays `lamports` out of the Hot Zone PDA (signed by its seeds).
fn pay_hot(
    hot_acc: &AccountView,
    prof: &AccountView,
    d: &[u8],
    to: &AccountView,
    lamports: u64,
) -> ProgramResult {
    let hb = [byte(d, p::HOT_BUMP)?];
    transfer(
        hot_acc,
        to,
        lamports,
        &[
            Seed::from(b"hot"),
            Seed::from(prof.address().as_ref()),
            Seed::from(&hb),
        ],
    )
}
fn spendable(hot_acc: &AccountView) -> Result<u64, ProgramError> {
    Ok(hot_acc.lamports().saturating_sub(rent(0)?))
}
fn init_account(
    payer: &AccountView,
    new: &AccountView,
    len: usize,
    seeds: &[Seed],
) -> ProgramResult {
    signer(payer)?;
    writable(payer)?;
    writable(new)?;
    // Never overwrite existing records; prefunding an empty system PDA remains harmless.
    if !new.owned_by(&SYSTEM) || new.data_len() != 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }
    let shortfall = rent(len)?.saturating_sub(new.lamports());
    transfer(payer, new, shortfall, &[])?;
    let signers = [Signer::from(seeds)];
    Allocate {
        account: new,
        space: len as u64,
    }
    .invoke_signed(&signers)?;
    Assign {
        account: new,
        owner: &ID,
    }
    .invoke_signed(&signers)
}
fn close_account(a: &mut [AccountView], from: usize, to: usize) -> ProgramResult {
    let src = account(a, from)?;
    let dst = account(a, to)?;
    writable(src)?;
    writable(dst)?;
    need(src.address() != dst.address(), Error::BadAccount)?;
    let balance = add(dst.lamports(), src.lamports())?;
    let src = a.get_mut(from).ok_or(ProgramError::NotEnoughAccountKeys)?;
    src.try_borrow_mut()?.fill(0);
    // close() sets length=0, lamports=0 and owner=system before any later instruction/CPI.
    src.close()?;
    a.get_mut(to)
        .ok_or(ProgramError::NotEnoughAccountKeys)?
        .set_lamports(balance);
    Ok(())
}
fn emit(tag: u8, fields: &[&[u8]]) -> ProgramResult {
    let mut d = [0u8; 160];
    put(&mut d, 0, &[tag])?;
    let mut len = 1usize;
    for field in fields {
        put(&mut d, len, field)?;
        len = len
            .checked_add(field.len())
            .ok_or(err(Error::MathOverflow))?;
    }
    #[cfg(target_os = "solana")]
    {
        let raw = [(d.as_ptr(), len as u64)];
        // SAFETY: the slice is initialized and lives through this syscall.
        unsafe { pinocchio::syscalls::sol_log_data(raw.as_ptr() as *const u8, 1) };
    }
    #[cfg(not(target_os = "solana"))]
    let _ = len;
    Ok(())
}
fn effective_mode(prof: &[u8], cfg: &[u8]) -> Result<u8, ProgramError> {
    let mode = byte(prof, p::MODE)?;
    if byte(prof, p::FOLLOW)? == 1 {
        // a mesh JD stays in force for this agent until its owner reboots it, even after the mesh level drops
        if u64_at(cfg, c::JD_EPOCH)? > u64_at(prof, p::ACK_EPOCH)? {
            return Ok(JD);
        }
        Ok(mode.max(byte(cfg, c::LEVEL)?))
    } else {
        Ok(mode)
    }
}
fn find_bump_config() -> Result<u8, ProgramError> {
    Ok(Address::find_program_address(&[b"config"], &ID).1)
}
fn bump_agents(cfg: &mut [u8], up: bool) -> ProgramResult {
    let n = u64_at(cfg, c::AGENTS)?;
    let n = if up { add(n, 1)? } else { sub(n, 1)? };
    put(cfg, c::AGENTS, &n.to_le_bytes())
}

pub fn process_instruction(pid: &Address, a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    need(pid == &ID, Error::Unauthorized)?;
    let (&op, args) = d.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    match op {
        0 => initialize(a, args),
        1 => register_agent(a, args),
        2 => update_policy(a, args),
        3 => spend(a, args),
        4 => set_mode(a, args),
        5 => evacuate(a),
        6 => withdraw(a, args),
        7 => close_agent(a),
        8 => report_incident(a, args),
        9 => set_level(a, args),
        10 => admin_pause(a, args),
        11 => admin_refund(a),
        12 => admin_close_start(a),
        13 => close_config(a),
        14 => set_admin(a, args),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn args_len(d: &[u8], n: usize) -> ProgramResult {
    if d.len() == n {
        Ok(())
    } else {
        Err(ProgramError::InvalidInstructionData)
    }
}

fn initialize(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 64)?;
    let authority = account(a, 0)?;
    signer(authority)?;
    writable(authority)?;
    writable(account(a, 1)?)?;
    system(account(a, 4)?)?;
    let program = account(a, 2)?;
    let pd = account(a, 3)?;
    need(
        program.address() == &ID && program.owned_by(&LOADER) && program.executable(),
        Error::Unauthorized,
    )?;
    let pd_key = Address::find_program_address(&[ID.as_ref()], &LOADER).0;
    need(
        pd.address() == &pd_key && pd.owned_by(&LOADER),
        Error::Unauthorized,
    )?;
    {
        let data = program.try_borrow()?;
        need(
            rd::<4>(&data, 0)? == 2u32.to_le_bytes() && key(&data, 4)? == pd_key,
            Error::Unauthorized,
        )?;
        let data = pd.try_borrow()?;
        need(
            rd::<4>(&data, 0)? == 3u32.to_le_bytes()
                && byte(&data, 12)? == 1
                && key(&data, 13)? == *authority.address(),
            Error::Unauthorized,
        )?;
    }
    let admin = key(d, 0)?;
    let mesh = key(d, 32)?;
    need(admin != ZERO && mesh != ZERO, Error::BadParams)?;
    let bump = [find(&[b"config"], account(a, 1)?)?];
    init_account(
        authority,
        account(a, 1)?,
        CONFIG_LEN,
        &[Seed::from(b"config"), Seed::from(&bump)],
    )?;
    let mut cfg = [0u8; CONFIG_LEN];
    put(&mut cfg, 0, &CONFIG_DISC)?;
    put(&mut cfg, c::ADMIN, admin.as_ref())?;
    put(&mut cfg, c::MESH, mesh.as_ref())?;
    put(&mut cfg, c::BUMP, &bump)?;
    put(&mut cfg, c::LEVEL_SINCE, &now()?.to_le_bytes())?;
    save_at(a, 1, &cfg)?;
    emit(14, &[admin.as_ref(), mesh.as_ref()])
}

// Validates the policy block and writes it into the profile. `hot_key` may never be a recipient or the route.
fn write_policy(prof: &mut [u8], d: &[u8], banned: &[Address; 3]) -> ProgramResult {
    let owner = key(prof, p::OWNER)?;
    let n = byte(d, a::N)? as usize;
    need(n <= MAX_RECIPIENTS, Error::BadParams)?;
    args_len(d, a::RECIPIENTS + n * 32)?;
    let agent = key(d, a::AGENT)?;
    let defender = key(d, a::DEFENDER)?;
    let route = key(d, a::ROUTE)?;
    let follow = byte(d, a::FOLLOW)?;
    let tx_w = u64_at(d, a::CAPS)?;
    let win_w = u64_at(d, a::CAPS + 8)?;
    let tx_a = u64_at(d, a::CAPS + 16)?;
    let win_a = u64_at(d, a::CAPS + 24)?;
    let window = u64_at(d, a::WINDOW)?;
    let expires = i64_at(d, a::EXPIRES)?;
    need(
        agent != ZERO
            && defender != ZERO
            && agent != owner
            && route != ZERO
            && !banned.contains(&route)
            && follow <= 1
            && (60..=30 * 86400).contains(&window)
            && tx_w <= win_w
            && tx_a <= win_a
            && tx_a <= tx_w
            && win_a <= win_w
            && expires >= 0,
        Error::BadParams,
    )?;
    put(prof, p::AGENT, agent.as_ref())?;
    put(prof, p::DEFENDER, defender.as_ref())?;
    put(prof, p::ROUTE, route.as_ref())?;
    put(prof, p::FOLLOW, &[follow])?;
    put(prof, p::CAP_TX, rd::<32>(d, a::CAPS)?.as_ref())?;
    put(prof, p::WINDOW, &window.to_le_bytes())?;
    put(prof, p::EXPIRES, &expires.to_le_bytes())?;
    put(prof, p::N, &[n as u8])?;
    put(prof, p::RECIPIENTS, &[0u8; MAX_RECIPIENTS * 32])?;
    for i in 0..n {
        let r = key(d, a::RECIPIENTS + i * 32)?;
        need(r != ZERO && !banned.contains(&r), Error::BadParams)?;
        put(prof, p::RECIPIENTS + i * 32, r.as_ref())?;
    }
    Ok(())
}

fn register_agent(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let id = rd::<32>(d, 0).map_err(|_| ProgramError::InvalidInstructionData)?;
    let policy = d.get(32..).ok_or(ProgramError::InvalidInstructionData)?;
    let owner = account(a, 0)?;
    signer(owner)?;
    writable(owner)?;
    let mut cfg = config(account(a, 1)?)?;
    writable(account(a, 1)?)?;
    writable(account(a, 3)?)?;
    system(account(a, 4)?)?;
    need(byte(&cfg, c::CLOSING)? == 0, Error::WindingDown)?;
    need(byte(&cfg, c::PAUSED)? == 0, Error::Paused)?;
    let owner_key = *owner.address();
    let bump = [find(&[b"agent", owner_key.as_ref(), &id], account(a, 2)?)?];
    let prof_key = *account(a, 2)?.address();
    let hb = [find(&[b"hot", prof_key.as_ref()], account(a, 3)?)?];
    money(account(a, 3)?, &[b"hot", prof_key.as_ref(), &hb])?;
    let hot_key = *account(a, 3)?.address();
    let mut prof = [0u8; PROFILE_LEN];
    put(&mut prof, 0, &PROFILE_DISC)?;
    put(&mut prof, p::OWNER, owner_key.as_ref())?;
    put(&mut prof, p::ID, &id)?;
    put(&mut prof, p::BUMP, &bump)?;
    put(&mut prof, p::HOT_BUMP, &hb)?;
    let t = now()?;
    put(&mut prof, p::WINDOW_START, &t.to_le_bytes())?;
    put(&mut prof, p::ACK_EPOCH, &rd::<8>(&cfg, c::JD_EPOCH)?)?;
    // every registration gets its own nonce range (counter<<40), so a spend signed for an earlier profile with the
    // same id can never land on a new one, even within the same second
    let reg = u32::from_le_bytes(rd(&cfg, c::REGISTRATIONS)?);
    put(&mut cfg, c::REGISTRATIONS, &reg.checked_add(1).ok_or(err(Error::MathOverflow))?.to_le_bytes())?;
    put(&mut prof, p::NONCE, &((reg as u64) << 40).to_le_bytes())?;
    let cfg_key = *account(a, 1)?.address();
    write_policy(&mut prof, policy, &[hot_key, prof_key, cfg_key])?;
    init_account(
        owner,
        account(a, 2)?,
        PROFILE_LEN,
        &[
            Seed::from(b"agent"),
            Seed::from(owner_key.as_ref()),
            Seed::from(&id),
            Seed::from(&bump),
        ],
    )?;
    // keep the Hot Zone rent-exempt so it can hold any amount
    transfer(
        owner,
        account(a, 3)?,
        rent(0)?.saturating_sub(account(a, 3)?.lamports()),
        &[],
    )?;
    save_at(a, 2, &prof)?;
    bump_agents(&mut cfg, true)?;
    save_at(a, 1, &cfg)?;
    emit(
        1,
        &[prof_key.as_ref(), owner_key.as_ref(), &rd::<32>(&prof, p::AGENT)?],
    )
}

fn update_policy(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    let mut prof = profile(account(a, 1)?)?;
    auth(&prof, p::OWNER, account(a, 0)?)?;
    let prof_key = *account(a, 1)?.address();
    let hot_key = derived(&[b"hot", prof_key.as_ref(), &[byte(&prof, p::HOT_BUMP)?]])?;
    let cfg_key = derived(&[b"config", &[find_bump_config()?]])?;
    let follow_before = byte(&prof, p::FOLLOW)?;
    write_policy(&mut prof, d, &[hot_key, prof_key, cfg_key])?;
    if follow_before == 0 && byte(&prof, p::FOLLOW)? == 1 {
        let cfg = config(account(a, 2)?)?;
        put(&mut prof, p::ACK_EPOCH, &rd::<8>(&cfg, c::JD_EPOCH)?)?;
    }
    save_at(a, 1, &prof)?;
    emit(2, &[prof_key.as_ref()])
}

fn spend(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 16)?;
    let amount = u64_at(d, 0)?;
    let nonce = u64_at(d, 8)?;
    let cfg = config(account(a, 1)?)?;
    let mut prof = profile(account(a, 2)?)?;
    auth(&prof, p::AGENT, account(a, 0)?)?;
    system(account(a, 5)?)?;
    hot(account(a, 3)?, account(a, 2)?, &prof)?;
    need(byte(&cfg, c::PAUSED)? == 0, Error::Paused)?;
    let mode = effective_mode(&prof, &cfg)?;
    need(mode < JD, Error::Lockdown)?;
    let t = now()?;
    let expires = i64_at(&prof, p::EXPIRES)?;
    need(expires == 0 || t < expires, Error::Expired)?;
    need(nonce == u64_at(&prof, p::NONCE)?, Error::BadNonce)?;
    let to = *account(a, 4)?.address();
    let n = byte(&prof, p::N)? as usize;
    let mut allowed = false;
    for i in 0..n.min(MAX_RECIPIENTS) {
        allowed |= key(&prof, p::RECIPIENTS + i * 32)? == to;
    }
    need(allowed, Error::BadRecipient)?;
    need(amount > 0, Error::BadParams)?;
    let window = u64_at(&prof, p::WINDOW)? as i64;
    let mut start = i64_at(&prof, p::WINDOW_START)?;
    let mut spent = u64_at(&prof, p::SPENT)?;
    if t >= start.checked_add(window).ok_or(err(Error::MathOverflow))? {
        start = t;
        spent = 0;
    }
    let caps = p::CAP_TX + 16 * mode as usize;
    spent = add(spent, amount)?;
    need(
        amount <= u64_at(&prof, caps)? && spent <= u64_at(&prof, caps + 8)?,
        Error::OverLimit,
    )?;
    need(amount <= spendable(account(a, 3)?)?, Error::InsufficientFunds)?;
    pay_hot(account(a, 3)?, account(a, 2)?, &prof, account(a, 4)?, amount)?;
    let next = add(nonce, 1)?;
    put(&mut prof, p::WINDOW_START, &start.to_le_bytes())?;
    put(&mut prof, p::SPENT, &spent.to_le_bytes())?;
    put(&mut prof, p::NONCE, &next.to_le_bytes())?;
    save_at(a, 2, &prof)?;
    emit(
        3,
        &[
            account(a, 2)?.address().as_ref(),
            to.as_ref(),
            &amount.to_le_bytes(),
            &nonce.to_le_bytes(),
            &[mode],
        ],
    )
}

fn set_mode(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 1)?;
    let mode = d[0];
    need(mode <= JD, Error::BadParams)?;
    let mut prof = profile(account(a, 1)?)?;
    let who = account(a, 0)?;
    signer(who)?;
    let old = byte(&prof, p::MODE)?;
    if who.address() == &key(&prof, p::OWNER)? {
        // owner may lower: the Zero Trust Reboot path; also acknowledges every mesh JD so far
        let cfg = config(account(a, 2)?)?;
        put(&mut prof, p::ACK_EPOCH, &rd::<8>(&cfg, c::JD_EPOCH)?)?;
    } else if who.address() == &key(&prof, p::DEFENDER)? {
        need(mode > old, Error::OnlyRaise)?;
    } else {
        return Err(err(Error::Unauthorized));
    }
    let who_key = *who.address();
    put(&mut prof, p::MODE, &[mode])?;
    save_at(a, 1, &prof)?;
    emit(
        4,
        &[account(a, 1)?.address().as_ref(), who_key.as_ref(), &[old, mode]],
    )
}

fn evacuate(a: &mut [AccountView]) -> ProgramResult {
    let cfg = config(account(a, 1)?)?;
    let prof = profile(account(a, 2)?)?;
    let who = account(a, 0)?;
    signer(who)?;
    need(
        who.address() == &key(&prof, p::OWNER)? || who.address() == &key(&prof, p::DEFENDER)?,
        Error::Unauthorized,
    )?;
    system(account(a, 5)?)?;
    hot(account(a, 3)?, account(a, 2)?, &prof)?;
    need(effective_mode(&prof, &cfg)? == JD, Error::NotLockdown)?;
    let route = account(a, 4)?;
    need(route.address() == &key(&prof, p::ROUTE)?, Error::BadAccount)?;
    let amount = spendable(account(a, 3)?)?;
    pay_hot(account(a, 3)?, account(a, 2)?, &prof, route, amount)?;
    emit(
        5,
        &[account(a, 2)?.address().as_ref(), route.address().as_ref(), &amount.to_le_bytes()],
    )
}

fn withdraw(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 8)?;
    let amount = u64_at(d, 0)?;
    let prof = profile(account(a, 1)?)?;
    auth(&prof, p::OWNER, account(a, 0)?)?;
    system(account(a, 3)?)?;
    hot(account(a, 2)?, account(a, 1)?, &prof)?;
    need(amount > 0, Error::BadParams)?;
    need(amount <= spendable(account(a, 2)?)?, Error::InsufficientFunds)?;
    pay_hot(account(a, 2)?, account(a, 1)?, &prof, account(a, 0)?, amount)?;
    emit(6, &[account(a, 1)?.address().as_ref(), &amount.to_le_bytes()])
}

// Empties the Hot Zone to the owner and closes the profile; rent goes to the owner.
fn settle_profile(a: &mut [AccountView], cfg_i: usize, prof_i: usize, hot_i: usize, owner_i: usize, sys_i: usize, to_i: usize) -> Result<u64, ProgramError> {
    let mut cfg = config(account(a, cfg_i)?)?;
    writable(account(a, cfg_i)?)?;
    let prof = profile(account(a, prof_i)?)?;
    need(account(a, owner_i)?.address() == &key(&prof, p::OWNER)?, Error::Unauthorized)?;
    system(account(a, sys_i)?)?;
    hot(account(a, hot_i)?, account(a, prof_i)?, &prof)?;
    let all = account(a, hot_i)?.lamports();
    pay_hot(account(a, hot_i)?, account(a, prof_i)?, &prof, account(a, to_i)?, all)?;
    close_account(a, prof_i, to_i)?;
    bump_agents(&mut cfg, false)?;
    save_at(a, cfg_i, &cfg)?;
    Ok(all)
}

fn close_agent(a: &mut [AccountView]) -> ProgramResult {
    let owner = account(a, 0)?;
    signer(owner)?;
    let prof_key = *account(a, 2)?.address();
    settle_profile(a, 1, 2, 3, 0, 4, 0)?;
    emit(7, &[prof_key.as_ref()])
}

fn report_incident(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 33)?;
    let mut prof = profile(account(a, 1)?)?;
    auth(&prof, p::DEFENDER, account(a, 0)?)?;
    let n = u32::from_le_bytes(rd(&prof, p::INCIDENTS)?).saturating_add(1);
    put(&mut prof, p::INCIDENTS, &n.to_le_bytes())?;
    save_at(a, 1, &prof)?;
    emit(8, &[account(a, 1)?.address().as_ref(), d])
}

fn set_level(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 1)?;
    let level = d[0];
    need(level <= JD, Error::BadParams)?;
    let mut cfg = config(account(a, 1)?)?;
    let who = account(a, 0)?;
    signer(who)?;
    need(
        who.address() == &key(&cfg, c::MESH)? || who.address() == &key(&cfg, c::ADMIN)?,
        Error::Unauthorized,
    )?;
    let old = byte(&cfg, c::LEVEL)?;
    put(&mut cfg, c::LEVEL, &[level])?;
    let t = now()?;
    put(&mut cfg, c::LEVEL_SINCE, &t.to_le_bytes())?;
    if level == JD {
        let e = add(u64_at(&cfg, c::JD_EPOCH)?, 1)?;
        put(&mut cfg, c::JD_EPOCH, &e.to_le_bytes())?;
    }
    let who_key = *who.address();
    save_at(a, 1, &cfg)?;
    emit(9, &[&[old, level], who_key.as_ref()])
}

fn admin_pause(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 1)?;
    need(d[0] <= 1, Error::BadParams)?;
    let mut cfg = config(account(a, 1)?)?;
    auth(&cfg, c::ADMIN, account(a, 0)?)?;
    need(d[0] == 1 || byte(&cfg, c::CLOSING)? == 0, Error::WindingDown)?;
    put(&mut cfg, c::PAUSED, &d[..1])?;
    save_at(a, 1, &cfg)?;
    emit(10, &[&d[..1]])
}

fn admin_refund(a: &mut [AccountView]) -> ProgramResult {
    let cfg = config(account(a, 1)?)?;
    auth(&cfg, c::ADMIN, account(a, 0)?)?;
    need(
        byte(&cfg, c::PAUSED)? == 1 || byte(&cfg, c::CLOSING)? == 1,
        Error::NotLockdown,
    )?;
    let prof_key = *account(a, 2)?.address();
    // an owner address that can never be credited (e.g. now executable) must not block wind-down
    let to = if account(a, 4)?.executable() { 0 } else { 4 };
    let to_key = *account(a, to)?.address();
    let all = settle_profile(a, 1, 2, 3, 4, 5, to)?;
    emit(11, &[prof_key.as_ref(), to_key.as_ref(), &all.to_le_bytes()])
}

fn admin_close_start(a: &mut [AccountView]) -> ProgramResult {
    let mut cfg = config(account(a, 1)?)?;
    auth(&cfg, c::ADMIN, account(a, 0)?)?;
    put(&mut cfg, c::CLOSING, &[1])?;
    put(&mut cfg, c::PAUSED, &[1])?;
    save_at(a, 1, &cfg)?;
    emit(12, &[])
}

fn close_config(a: &mut [AccountView]) -> ProgramResult {
    let cfg = config(account(a, 1)?)?;
    auth(&cfg, c::ADMIN, account(a, 0)?)?;
    need(byte(&cfg, c::CLOSING)? == 1, Error::NotLockdown)?;
    need(u64_at(&cfg, c::AGENTS)? == 0, Error::AgentsOpen)?;
    close_account(a, 1, 0)?;
    emit(13, &[])
}

fn set_admin(a: &mut [AccountView], d: &[u8]) -> ProgramResult {
    args_len(d, 64)?;
    let mut cfg = config(account(a, 1)?)?;
    auth(&cfg, c::ADMIN, account(a, 0)?)?;
    let admin = key(d, 0)?;
    let mesh = key(d, 32)?;
    need(admin != ZERO && mesh != ZERO, Error::BadParams)?;
    put(&mut cfg, c::ADMIN, admin.as_ref())?;
    put(&mut cfg, c::MESH, mesh.as_ref())?;
    save_at(a, 1, &cfg)?;
    emit(14, &[admin.as_ref(), mesh.as_ref()])
}
