// Copyright 2022-2024 Protocol Labs
// SPDX-License-Identifier: Apache-2.0, MIT

use std::path::Path;

use anyhow::Context;
use base64::Engine;
use bytes::Bytes;
use fendermint_actor_objectstore::{
    ObjectDeleteParams, ObjectGetParams, ObjectKind, ObjectListParams, ObjectPutParams,
};
use fendermint_crypto::SecretKey;
use fendermint_vm_actor_interface::{accumulator, eam, evm, objectstore};
use fendermint_vm_message::signed::Object;
use fendermint_vm_message::{chain::ChainMessage, signed::SignedMessage};
use fvm_ipld_encoding::{BytesSer, RawBytes};
use fvm_shared::{
    address::Address, chainid::ChainID, econ::TokenAmount, message::Message, MethodNum, METHOD_SEND,
};

use crate::B64_ENGINE;

/// Factory methods for transaction payload construction.
///
/// It assumes the sender is an `f1` type address, it won't work with `f410` addresses.
/// For those one must use the Ethereum API, with a suitable client library such as [ethers].
pub struct MessageFactory {
    addr: Address,
    sequence: u64,
}

impl MessageFactory {
    pub fn new(addr: Address, sequence: u64) -> Self {
        Self { addr, sequence }
    }

    pub fn address(&self) -> &Address {
        &self.addr
    }

    /// Set the sequence to an arbitrary value.
    pub fn set_sequence(&mut self, sequence: u64) {
        self.sequence = sequence;
    }

    pub fn transaction(
        &mut self,
        to: Address,
        method_num: MethodNum,
        params: RawBytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> Message {
        let msg = Message {
            version: Default::default(), // TODO: What does this do?
            from: self.addr,
            to,
            sequence: self.sequence,
            value,
            method_num,
            params,
            gas_limit: gas_params.gas_limit,
            gas_fee_cap: gas_params.gas_fee_cap,
            gas_premium: gas_params.gas_premium,
        };

        self.sequence += 1;

        msg
    }

    pub fn fevm_create(
        &mut self,
        contract: Bytes,
        constructor_args: Bytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<Message> {
        let initcode = [contract.to_vec(), constructor_args.to_vec()].concat();
        let initcode = RawBytes::serialize(BytesSer(&initcode))?;
        Ok(self.transaction(
            eam::EAM_ACTOR_ADDR,
            eam::Method::CreateExternal as u64,
            initcode,
            value,
            gas_params,
        ))
    }

    pub fn fevm_invoke(
        &mut self,
        contract: Address,
        calldata: Bytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<Message> {
        let calldata = RawBytes::serialize(BytesSer(&calldata))?;
        Ok(self.transaction(
            contract,
            evm::Method::InvokeContract as u64,
            calldata,
            value,
            gas_params,
        ))
    }

    pub fn fevm_call(
        &mut self,
        contract: Address,
        calldata: Bytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<Message> {
        let msg = self.fevm_invoke(contract, calldata, value, gas_params)?;

        // Roll back the sequence, we don't really want to invoke anything.
        self.set_sequence(msg.sequence);

        Ok(msg)
    }
}
/// Wrapper for MessageFactory which generates signed messages
///
/// It assumes the sender is an `f1` type address, it won't work with `f410` addresses.
/// For those one must use the Ethereum API, with a suitable client library such as [ethers].
pub struct SignedMessageFactory {
    inner: MessageFactory,
    sk: SecretKey,
    chain_id: ChainID,
}

impl SignedMessageFactory {
    /// Create a factor from a secret key and its corresponding address, which could be a delegated one.
    pub fn new(sk: SecretKey, addr: Address, sequence: u64, chain_id: ChainID) -> Self {
        Self {
            inner: MessageFactory::new(addr, sequence),
            sk,
            chain_id,
        }
    }

    /// Treat the secret key as an f1 type account.
    pub fn new_secp256k1(sk: SecretKey, sequence: u64, chain_id: ChainID) -> Self {
        let pk = sk.public_key();
        let addr = Address::new_secp256k1(&pk.serialize()).expect("public key is 65 bytes");
        Self::new(sk, addr, sequence, chain_id)
    }

    /// Convenience method to read the secret key from a file, expected to be in Base64 format.
    pub fn read_secret_key(sk: &Path) -> anyhow::Result<SecretKey> {
        let b64 = std::fs::read_to_string(sk).context("failed to read secret key")?;
        let bz: Vec<u8> = B64_ENGINE
            .decode(b64)
            .context("failed to parse base64 string")?;
        let sk = SecretKey::try_from(bz)?;
        Ok(sk)
    }

    /// Convenience method to serialize a [`ChainMessage`] for inclusion in a Tendermint transaction.
    pub fn serialize(message: &ChainMessage) -> anyhow::Result<Vec<u8>> {
        Ok(fvm_ipld_encoding::to_vec(message)?)
    }

    /// Actor address.
    pub fn address(&self) -> &Address {
        self.inner.address()
    }

    /// Transfer tokens to another account.
    pub fn transfer(
        &mut self,
        to: Address,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<ChainMessage> {
        self.transaction(to, METHOD_SEND, Default::default(), value, gas_params, None)
    }

    /// Send a message to an actor.
    pub fn transaction(
        &mut self,
        to: Address,
        method_num: MethodNum,
        params: RawBytes,
        value: TokenAmount,
        gas_params: GasParams,
        object: Option<Object>,
    ) -> anyhow::Result<ChainMessage> {
        let message = self
            .inner
            .transaction(to, method_num, params, value, gas_params);
        let signed = SignedMessage::new_secp256k1(message, object, &self.sk, &self.chain_id)?;
        let chain = ChainMessage::Signed(signed);
        Ok(chain)
    }

    /// Put an object into an object store.
    pub fn os_put(
        &mut self,
        params: ObjectPutParams,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<ChainMessage> {
        let object = match &params.kind {
            ObjectKind::Internal(_) => None,
            ObjectKind::External(cid) => Some(Object::new(params.key.clone(), *cid)),
        };
        let params = RawBytes::serialize(params)?;
        let message = self.transaction(
            objectstore::OBJECTSTORE_ACTOR_ADDR,
            fendermint_actor_objectstore::Method::PutObject as u64,
            params,
            value,
            gas_params,
            object,
        )?;
        Ok(message)
    }

    /// Delete an object from an object store.
    pub fn os_delete(
        &mut self,
        params: ObjectDeleteParams,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<ChainMessage> {
        let params = RawBytes::serialize(params)?;
        let message = self.transaction(
            objectstore::OBJECTSTORE_ACTOR_ADDR,
            fendermint_actor_objectstore::Method::DeleteObject as u64,
            params,
            value,
            gas_params,
            None,
        )?;
        Ok(message)
    }

    /// Get an object from an object store. This will not create a transaction.
    pub fn os_get(
        &mut self,
        params: ObjectGetParams,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<Message> {
        let params = RawBytes::serialize(params)?;
        let message = self.transaction(
            objectstore::OBJECTSTORE_ACTOR_ADDR,
            fendermint_actor_objectstore::Method::GetObject as u64,
            params,
            value,
            gas_params,
            None,
        )?;

        let message = if let ChainMessage::Signed(signed) = message {
            signed.into_message()
        } else {
            panic!("unexpected message type: {message:?}");
        };

        // Roll back the sequence, we don't really want to invoke anything.
        self.inner.set_sequence(message.sequence);

        Ok(message)
    }

    /// List objects in an object store. This will not create a transaction.
    pub fn os_list(
        &mut self,
        params: ObjectListParams,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<Message> {
        let params = RawBytes::serialize(params)?;
        let message = self.transaction(
            objectstore::OBJECTSTORE_ACTOR_ADDR,
            fendermint_actor_objectstore::Method::ListObjects as u64,
            params,
            value,
            gas_params,
            None,
        )?;

        let message = if let ChainMessage::Signed(signed) = message {
            signed.into_message()
        } else {
            panic!("unexpected message type: {message:?}");
        };

        // Roll back the sequence, we don't really want to invoke anything.
        self.inner.set_sequence(message.sequence);

        Ok(message)
    }

    /// Push an event to an accumulator.
    pub fn acc_push(
        &mut self,
        event: Bytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<ChainMessage> {
        let params = RawBytes::serialize(event.to_vec())?;
        let message = self.transaction(
            accumulator::ACCUMULATOR_ACTOR_ADDR,
            fendermint_actor_accumulator::Method::Push as u64,
            params,
            value,
            gas_params,
            None,
        )?;
        Ok(message)
    }

    /// Get the object stored at a given index in an accumulator. This will not create a transaction.
    pub fn acc_get_at(
        &mut self,
        value: TokenAmount,
        gas_params: GasParams,
        index: u64,
    ) -> anyhow::Result<Message> {
        let params = RawBytes::serialize(index)?;
        let message = self.transaction(
            accumulator::ACCUMULATOR_ACTOR_ADDR,
            fendermint_actor_accumulator::Method::Get as u64,
            params,
            value,
            gas_params,
            None,
        )?;

        let message = if let ChainMessage::Signed(signed) = message {
            signed.into_message()
        } else {
            panic!("unexpected message type: {message:?}");
        };

        // Roll back the sequence, we don't really want to invoke anything.
        self.inner.set_sequence(message.sequence);

        Ok(message)
    }

    /// Get the root commitment in an accumulator. This will not create a transaction.
    pub fn acc_root(
        &mut self,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<Message> {
        let message = self.transaction(
            accumulator::ACCUMULATOR_ACTOR_ADDR,
            fendermint_actor_accumulator::Method::Root as u64,
            RawBytes::default(),
            value,
            gas_params,
            None,
        )?;

        let message = if let ChainMessage::Signed(signed) = message {
            signed.into_message()
        } else {
            panic!("unexpected message type: {message:?}");
        };

        // Roll back the sequence, we don't really want to invoke anything.
        self.inner.set_sequence(message.sequence);

        Ok(message)
    }

    /// Deploy a FEVM contract.
    pub fn fevm_create(
        &mut self,
        contract: Bytes,
        constructor_args: Bytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<ChainMessage> {
        let initcode = [contract.to_vec(), constructor_args.to_vec()].concat();
        let initcode = RawBytes::serialize(BytesSer(&initcode))?;
        let message = self.transaction(
            eam::EAM_ACTOR_ADDR,
            eam::Method::CreateExternal as u64,
            initcode,
            value,
            gas_params,
            None,
        )?;
        Ok(message)
    }

    /// Invoke a method on a FEVM contract.
    pub fn fevm_invoke(
        &mut self,
        contract: Address,
        calldata: Bytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<ChainMessage> {
        let calldata = RawBytes::serialize(BytesSer(&calldata))?;
        let message = self.transaction(
            contract,
            evm::Method::InvokeContract as u64,
            calldata,
            value,
            gas_params,
            None,
        )?;
        Ok(message)
    }

    /// Create a message for a read-only operation.
    pub fn fevm_call(
        &mut self,
        contract: Address,
        calldata: Bytes,
        value: TokenAmount,
        gas_params: GasParams,
    ) -> anyhow::Result<Message> {
        let msg = self.fevm_invoke(contract, calldata, value, gas_params)?;

        let msg = if let ChainMessage::Signed(signed) = msg {
            signed.into_message()
        } else {
            panic!("unexpected message type: {msg:?}");
        };

        // Roll back the sequence, we don't really want to invoke anything.
        self.inner.set_sequence(msg.sequence);

        Ok(msg)
    }
}

#[derive(Clone, Debug)]
pub struct GasParams {
    /// Maximum amount of gas that can be charged.
    pub gas_limit: u64,
    /// Price of gas.
    ///
    /// Any discrepancy between this and the base fee is paid for
    /// by the validator who puts the transaction into the block.
    pub gas_fee_cap: TokenAmount,
    /// Gas premium.
    pub gas_premium: TokenAmount,
}
