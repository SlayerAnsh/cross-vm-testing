use anchor_lang::prelude::*;

declare_id!("54ex8sgs6H3Y2NssU3CWdBhySk9q5Gqc4MMtPYTtJzC5");

pub const PING_MSG: &str = "ping";
pub const PONG_ACK: &str = "pong";
pub const MAX_CHAIN_ID_LEN: usize = 32;
pub const MAX_PORT_LEN: usize = 128;

#[program]
pub mod ping_pong {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, chain_id: String) -> Result<()> {
        require!(
            chain_id.len() <= MAX_CHAIN_ID_LEN,
            PingPongError::ChainIdTooLong
        );
        let state = &mut ctx.accounts.ping_pong_state;
        state.chain_id = chain_id;
        state.next_sequence = 0;
        state.pings_sent = 0;
        state.pongs_received = 0;
        state.bump = ctx.bumps.ping_pong_state;
        Ok(())
    }

    pub fn ping(ctx: Context<Update>, destination_port: String) -> Result<()> {
        require!(
            destination_port.len() <= MAX_PORT_LEN,
            PingPongError::PortTooLong
        );
        let state = &mut ctx.accounts.ping_pong_state;
        let source_port = format_port(&state.chain_id, &crate::ID.to_string());
        let sequence = state.next_sequence;

        state.pings_sent = state.pings_sent.saturating_add(1);
        state.next_sequence = state.next_sequence.saturating_add(1);

        emit!(SendPacket {
            source_port,
            destination_port,
            packet_sequence: sequence,
            msg: PING_MSG.to_string(),
        });
        Ok(())
    }

    pub fn receive_packet(
        ctx: Context<Update>,
        source_port: String,
        destination_port: String,
        sequence: u64,
        msg: String,
    ) -> Result<()> {
        let state = &ctx.accounts.ping_pong_state;
        let self_port = format_port(&state.chain_id, &crate::ID.to_string());
        require!(
            destination_port == self_port,
            PingPongError::InvalidDestinationPort
        );
        require!(msg == PING_MSG, PingPongError::InvalidPacketMessage);

        emit!(ReceivePacket {
            source_port: source_port.clone(),
            destination_port: destination_port.clone(),
            packet_sequence: sequence,
        });
        emit!(WriteAcknowledgement {
            source_port,
            destination_port,
            packet_sequence: sequence,
            msg: PING_MSG.to_string(),
            ack: PONG_ACK.to_string(),
        });
        Ok(())
    }

    pub fn acknowledge_packet(
        ctx: Context<Update>,
        source_port: String,
        destination_port: String,
        sequence: u64,
    ) -> Result<()> {
        let state = &mut ctx.accounts.ping_pong_state;
        state.pongs_received = state.pongs_received.saturating_add(1);

        emit!(AcknowledgePacket {
            source_port,
            destination_port,
            packet_sequence: sequence,
        });
        Ok(())
    }
}

fn format_port(chain_id: &str, address: &str) -> String {
    format!("{chain_id}.{address}")
}

#[account]
#[derive(InitSpace)]
pub struct PingPongState {
    #[max_len(32)]
    pub chain_id: String,
    pub next_sequence: u64,
    pub pings_sent: u64,
    pub pongs_received: u64,
    pub bump: u8,
}

#[event]
pub struct SendPacket {
    pub source_port: String,
    pub destination_port: String,
    pub packet_sequence: u64,
    pub msg: String,
}

#[event]
pub struct ReceivePacket {
    pub source_port: String,
    pub destination_port: String,
    pub packet_sequence: u64,
}

#[event]
pub struct WriteAcknowledgement {
    pub source_port: String,
    pub destination_port: String,
    pub packet_sequence: u64,
    pub msg: String,
    pub ack: String,
}

#[event]
pub struct AcknowledgePacket {
    pub source_port: String,
    pub destination_port: String,
    pub packet_sequence: u64,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = user,
        space = 8 + PingPongState::INIT_SPACE,
        seeds = [b"ping_pong", user.key().as_ref()],
        bump
    )]
    pub ping_pong_state: Account<'info, PingPongState>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Update<'info> {
    #[account(
        mut,
        seeds = [b"ping_pong", user.key().as_ref()],
        bump = ping_pong_state.bump,
    )]
    pub ping_pong_state: Account<'info, PingPongState>,
    pub user: Signer<'info>,
}

#[error_code]
pub enum PingPongError {
    #[msg("chain id exceeds maximum length")]
    ChainIdTooLong,
    #[msg("port exceeds maximum length")]
    PortTooLong,
    #[msg("invalid destination port")]
    InvalidDestinationPort,
    #[msg("invalid packet message")]
    InvalidPacketMessage,
}
