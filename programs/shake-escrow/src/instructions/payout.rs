use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, TokenAccount, Transfer};

use crate::error::ShakeError;

/// Splits a pot of two equal stakes between the winner and the fee account. The fee is
/// floor(pot × fee_bps / 10,000), computed through u128, and the winner gets the rest, so the
/// rounding remainder rides with the winner and payout + fee equals the pot to the base unit.
/// Two stakes of 33,333,333 at 300 bps make a pot of 66,666,666, a fee of 1,999,999 and a
/// payout of 64,666,667; `split_pot_rounds_toward_the_winner` below asserts those figures.
pub fn split_pot(stake: u64, fee_bps: u16) -> Result<(u64, u64)> {
    let pot = stake.checked_mul(2).ok_or(ShakeError::MathOverflow)?;
    let fee = u64::try_from(
        (pot as u128)
            .checked_mul(fee_bps as u128)
            .ok_or(ShakeError::MathOverflow)?
            / 10_000u128,
    )
    .map_err(|_| ShakeError::MathOverflow)?;
    let payout = pot.checked_sub(fee).ok_or(ShakeError::MathOverflow)?;
    Ok((payout, fee))
}

/// Pays the winner and the fee, sweeps anything else in the vault to the fee account, and
/// closes the vault with its rent to the collector. Sweeping first is what lets the vault
/// always close: a donation becomes a tip instead of a balance that blocks the close. The
/// vault's authority is the wager PDA, so every transfer signs with the wager's seeds.
/// Returns the surplus swept.
pub fn pay_winner_and_close_vault<'info>(
    wager: AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
    vault: &mut Account<'info, TokenAccount>,
    winner_token: AccountInfo<'info>,
    fee_token: AccountInfo<'info>,
    rent_collector: AccountInfo<'info>,
    payout: u64,
    fee: u64,
) -> Result<u64> {
    token::transfer(
        CpiContext::new_with_signer(
            token::ID,
            Transfer {
                from: vault.to_account_info(),
                to: winner_token,
                authority: wager.clone(),
            },
            signer_seeds,
        ),
        payout,
    )?;
    if fee > 0 {
        token::transfer(
            CpiContext::new_with_signer(
                token::ID,
                Transfer {
                    from: vault.to_account_info(),
                    to: fee_token.clone(),
                    authority: wager.clone(),
                },
                signer_seeds,
            ),
            fee,
        )?;
    }
    vault.reload()?;
    let surplus = vault.amount;
    if surplus > 0 {
        token::transfer(
            CpiContext::new_with_signer(
                token::ID,
                Transfer {
                    from: vault.to_account_info(),
                    to: fee_token,
                    authority: wager.clone(),
                },
                signer_seeds,
            ),
            surplus,
        )?;
    }
    token::close_account(CpiContext::new_with_signer(
        token::ID,
        CloseAccount {
            account: vault.to_account_info(),
            destination: rent_collector,
            authority: wager,
        },
        signer_seeds,
    ))?;
    Ok(surplus)
}

#[cfg(test)]
mod tests {
    use super::split_pot;

    #[test]
    fn split_pot_rounds_toward_the_winner() {
        let (payout, fee) = split_pot(33_333_333, 300).unwrap();
        assert_eq!((payout, fee), (64_666_667, 1_999_999));
        let pot: u128 = 66_666_666;
        assert_eq!(u128::from(fee), pot * 300 / 10_000);
        assert_eq!(u128::from(payout + fee), pot);
    }

    #[test]
    fn split_pot_refuses_a_pot_past_u64() {
        assert!(split_pot(u64::MAX / 2, 1_000).is_ok());
        assert!(split_pot(u64::MAX / 2 + 1, 1_000).is_err());
    }
}
