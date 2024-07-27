use std::io::Write;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use cosmwasm_schema::cw_serde;
use cosmwasm_std::Binary;
use pyth_wormhole_attester_sdk::ErrBox;

use crate::state::PythDataSource;

const PYTH_GOVERNANCE_MAGIC: &[u8] = b"PTGM";

/// The type of contract that can accept a governance instruction.
#[cw_serde]
#[repr(u8)]
pub enum GovernanceModule {
    /// The PythNet executor contract. Messages sent to the
    Executor = 0,
    /// A target chain contract (like this one!)
    Target = 1,
}

impl GovernanceModule {
    pub fn from_u8(x: u8) -> Result<GovernanceModule, ErrBox> {
        match x {
            0 => Ok(GovernanceModule::Executor),
            1 => Ok(GovernanceModule::Target),
            _ => Err(format!("Invalid governance module: {x}",).into()),
        }
    }

    pub fn to_u8(&self) -> u8 {
        match &self {
            GovernanceModule::Executor => 0,
            GovernanceModule::Target => 1,
        }
    }
}

/// The action to perform to change the state of the target chain contract.
///
/// Note that the order of the enum cannot be changed, as the integer representation of
/// each field must be preserved for backward compatibility.
#[cw_serde]
#[repr(u8)]
pub enum GovernanceAction {
    /// Set the set of authorized emitters for price update messages.
    SetDataSources { data_sources: Vec<PythDataSource> }, // 2
}

#[cw_serde]
pub struct GovernanceInstruction {
    pub module: GovernanceModule,
    pub action: GovernanceAction,
    pub target_chain_id: u16,
}

impl GovernanceInstruction {
    pub fn deserialize(mut bytes: impl ReadBytesExt) -> Result<Self, ErrBox> {
        let mut magic_vec = vec![0u8; PYTH_GOVERNANCE_MAGIC.len()];
        bytes.read_exact(magic_vec.as_mut_slice())?;

        if magic_vec.as_slice() != PYTH_GOVERNANCE_MAGIC {
            return Err(format!(
                "Invalid magic {magic_vec:02X?}, expected {PYTH_GOVERNANCE_MAGIC:02X?}",
            )
            .into());
        }

        let module_num = bytes.read_u8()?;
        let module = GovernanceModule::from_u8(module_num)?;

        let action_type: u8 = bytes.read_u8()?;
        let target_chain_id: u16 = bytes.read_u16::<BigEndian>()?;

        let action: Result<GovernanceAction, String> = match action_type {
            2 => {
                let num_data_sources = bytes.read_u8()?;
                let mut data_sources: Vec<PythDataSource> = vec![];
                for _ in 0..num_data_sources {
                    let chain_id = bytes.read_u16::<BigEndian>()?;
                    let mut emitter_address: [u8; 32] = [0; 32];
                    bytes.read_exact(&mut emitter_address)?;

                    data_sources.push(PythDataSource {
                        emitter: Binary::from(&emitter_address),
                        chain_id,
                    });
                }

                Ok(GovernanceAction::SetDataSources { data_sources })
            }

            _ => Err(format!("Unknown governance action type: {action_type}",)),
        };

        // Check that we're at the end of the buffer (to ensure that this contract knows how to
        // interpret every field in the governance message). The logic is a little janky
        // but seems to be the simplest way to check that the reader is at EOF.
        let mut next_byte = [0_u8; 1];
        let read_result = bytes.read(&mut next_byte);
        match read_result {
            Ok(0) => (),
            _ => Err("Governance action had an unexpectedly long payload.".to_string())?,
        }

        Ok(GovernanceInstruction {
            module,
            action: action?,
            target_chain_id,
        })
    }

    pub fn serialize(&self) -> Result<Vec<u8>, ErrBox> {
        let mut buf = vec![];

        buf.write_all(PYTH_GOVERNANCE_MAGIC)?;
        buf.write_u8(self.module.to_u8())?;

        match &self.action {
            GovernanceAction::SetDataSources { data_sources } => {
                buf.write_u8(2)?;
                buf.write_u16::<BigEndian>(self.target_chain_id)?;
                buf.write_u8(u8::try_from(data_sources.len())?)?;
                for data_source in data_sources {
                    buf.write_u16::<BigEndian>(data_source.chain_id)?;

                    // The message format expects emitter addresses to be 32 bytes.
                    // However, we don't maintain this invariant in the rust code (and we violate it in the tests).
                    // This check gives you a reasonable error message if you happen to violate it in the tests.
                    if data_source.emitter.len() != 32 {
                        Err("Emitter addresses must be 32 bytes")?
                    }

                    buf.write_all(data_source.emitter.as_slice())?;
                }
            }
        }

        Ok(buf)
    }
}

#[cfg(test)]
mod test {

    #[test]
    fn test_payload_wrong_size() {}
}
