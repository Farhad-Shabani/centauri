// Copyright 2022 ComposableFi
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! This module implements the processing logic for ICS4 (channel) messages.

use crate::{
	core::{
		ics04_channel::{
			channel::ChannelEnd,
			error::Error,
			msgs::{ChannelMsg, PacketMsg},
			packet::PacketResult,
		},
		ics24_host::identifier::{ChannelId, PortId},
		ics26_routing::context::{
			Ics26Context, ModuleId, ModuleOutputBuilder, ReaderContext, Router,
		},
	},
	handler::{HandlerOutput, HandlerOutputBuilder},
};
use alloc::string::ToString;
use core::fmt::Debug;

pub mod acknowledgement;
pub mod chan_close_confirm;
pub mod chan_close_init;
pub mod chan_open_ack;
pub mod chan_open_confirm;
pub mod chan_open_init;
pub mod chan_open_try;
pub mod recv_packet;
pub mod send_packet;
pub mod timeout;
pub mod timeout_on_close;
pub mod verify;
pub mod write_acknowledgement;

/// Defines the possible states of a channel identifier in a `ChannelResult`.
#[derive(Clone, Debug)]
pub enum ChannelIdState {
	/// Specifies that the channel handshake handler allocated a new channel identifier. This
	/// happens during the processing of either the `MsgChannelOpenInit` or `MsgChannelOpenTry`.
	Generated,

	/// Specifies that the handler reused a previously-allocated channel identifier.
	Reused,
}

#[derive(Clone, Debug)]
pub struct ChannelResult {
	pub port_id: PortId,
	pub channel_id: ChannelId,
	pub channel_id_state: ChannelIdState,
	pub channel_end: ChannelEnd,
}

pub fn channel_validate<Ctx>(ctx: &Ctx, msg: &ChannelMsg) -> Result<ModuleId, Error>
where
	Ctx: Ics26Context,
{
	let module_id = msg.lookup_module(ctx)?;
	if ctx.router().has_route(&module_id) {
		Ok(module_id)
	} else {
		Err(Error::route_not_found())
	}
}

/// General entry point for processing any type of message related to the ICS4 channel open and
/// channel close handshake protocols.
pub fn channel_dispatch<Ctx>(
	ctx: &Ctx,
	msg: &ChannelMsg,
) -> Result<(HandlerOutputBuilder<()>, ChannelResult), Error>
where
	Ctx: ReaderContext,
{
	let output = match msg {
		ChannelMsg::ChannelOpenInit(msg) => chan_open_init::process(ctx, msg),
		ChannelMsg::ChannelOpenTry(msg) => chan_open_try::process::<_>(ctx, msg),
		ChannelMsg::ChannelOpenAck(msg) => chan_open_ack::process::<_>(ctx, msg),
		ChannelMsg::ChannelOpenConfirm(msg) => chan_open_confirm::process::<_>(ctx, msg),
		ChannelMsg::ChannelCloseInit(msg) => chan_close_init::process(ctx, msg),
		ChannelMsg::ChannelCloseConfirm(msg) => chan_close_confirm::process::<_>(ctx, msg),
	}?;
	let HandlerOutput { result, log, events } = output;
	let builder = HandlerOutput::builder().with_log(log).with_events(events);
	Ok((builder, result))
}

pub fn channel_callback<Ctx>(
	ctx: &mut Ctx,
	module_id: &ModuleId,
	msg: &ChannelMsg,
	mut result: ChannelResult,
	module_output: &mut ModuleOutputBuilder,
) -> Result<ChannelResult, Error>
where
	Ctx: Ics26Context,
{
	// Get an immutable context for module callbacks
	let ctx_clone = ctx.clone();
	let cb = ctx.router_mut().get_route_mut(module_id).ok_or_else(Error::route_not_found)?;

	match msg {
		ChannelMsg::ChannelOpenInit(msg) => cb.on_chan_open_init(
			&ctx_clone,
			module_output,
			msg.channel.ordering,
			&msg.channel.connection_hops,
			&msg.port_id,
			&result.channel_id,
			msg.channel.counterparty(),
			&msg.channel.version,
		)?,
		ChannelMsg::ChannelOpenTry(msg) => {
			let version = cb.on_chan_open_try(
				&ctx_clone,
				module_output,
				msg.channel.ordering,
				&msg.channel.connection_hops,
				&msg.port_id,
				&result.channel_id,
				msg.channel.counterparty(),
				msg.channel.version(),
				&msg.counterparty_version,
			)?;
			result.channel_end.version = version;
		},
		ChannelMsg::ChannelOpenAck(msg) => cb.on_chan_open_ack(
			&ctx_clone,
			module_output,
			&msg.port_id,
			&result.channel_id,
			&msg.counterparty_version,
		)?,
		ChannelMsg::ChannelOpenConfirm(msg) =>
			cb.on_chan_open_confirm(&ctx_clone, module_output, &msg.port_id, &result.channel_id)?,
		ChannelMsg::ChannelCloseInit(msg) =>
			cb.on_chan_close_init(&ctx_clone, module_output, &msg.port_id, &result.channel_id)?,
		ChannelMsg::ChannelCloseConfirm(msg) =>
			cb.on_chan_close_confirm(&ctx_clone, module_output, &msg.port_id, &result.channel_id)?,
	}
	Ok(result)
}

pub fn get_module_for_packet_msg<Ctx>(ctx: &Ctx, msg: &PacketMsg) -> Result<ModuleId, Error>
where
	Ctx: Ics26Context,
{
	let module_id = match msg {
		PacketMsg::RecvPacket(msg) => ctx
			.lookup_module_by_port(&msg.packet.destination_port)
			.map_err(Error::ics05_port)?,
		PacketMsg::AckPacket(msg) =>
			ctx.lookup_module_by_port(&msg.packet.source_port).map_err(Error::ics05_port)?,
		PacketMsg::ToPacket(msg) =>
			ctx.lookup_module_by_port(&msg.packet.source_port).map_err(Error::ics05_port)?,
		PacketMsg::ToClosePacket(msg) =>
			ctx.lookup_module_by_port(&msg.packet.source_port).map_err(Error::ics05_port)?,
	};

	if ctx.router().has_route(&module_id) {
		Ok(module_id)
	} else {
		Err(Error::route_not_found())
	}
}

/// Dispatcher for processing any type of message related to the ICS4 packet protocols.
pub fn packet_dispatch<Ctx>(
	ctx: &Ctx,
	msg: &PacketMsg,
) -> Result<(HandlerOutputBuilder<()>, PacketResult), Error>
where
	Ctx: ReaderContext,
{
	let output = match msg {
		PacketMsg::RecvPacket(msg) => recv_packet::process::<_>(ctx, msg),
		PacketMsg::AckPacket(msg) => acknowledgement::process::<_>(ctx, msg),
		PacketMsg::ToPacket(msg) => timeout::process::<_>(ctx, msg),
		PacketMsg::ToClosePacket(msg) => timeout_on_close::process::<_>(ctx, msg),
	}?;
	let HandlerOutput { result, log, events } = output;
	let builder = HandlerOutput::builder().with_log(log).with_events(events);
	Ok((builder, result))
}

pub fn packet_callback<Ctx>(
	ctx: &mut Ctx,
	module_id: &ModuleId,
	msg: &PacketMsg,
	module_output: &mut ModuleOutputBuilder,
) -> Result<(), Error>
where
	Ctx: Ics26Context,
{
	// Get an immutable context for module callbacks
	let ctx_clone = ctx.clone();
	let cb = ctx.router_mut().get_route_mut(module_id).ok_or_else(Error::route_not_found)?;

	match msg {
		PacketMsg::RecvPacket(msg) => {
			cb.on_recv_packet(&ctx_clone, module_output, &msg.packet, &msg.signer)
				.map_err(|e| Error::app_module(e.to_string()))?;
		},
		PacketMsg::AckPacket(msg) => cb.on_acknowledgement_packet(
			&ctx_clone,
			module_output,
			&msg.packet,
			&msg.acknowledgement,
			&msg.signer,
		)?,
		PacketMsg::ToPacket(msg) =>
			cb.on_timeout_packet(&ctx_clone, module_output, &msg.packet, &msg.signer)?,
		PacketMsg::ToClosePacket(msg) =>
			cb.on_timeout_packet(&ctx_clone, module_output, &msg.packet, &msg.signer)?,
	};
	Ok(())
}
