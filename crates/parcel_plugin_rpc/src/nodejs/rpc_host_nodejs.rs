use std::sync::mpsc::channel;
use std::sync::Arc;

use napi::threadsafe_function::ThreadSafeCallContext;
use napi::threadsafe_function::ThreadsafeFunction;
use napi::threadsafe_function::ThreadsafeFunctionCallMode;
use napi::Env;
use napi::JsFunction;
use napi::JsUnknown;
use napi::Status;

use crate::RpcConnectionRef;
use crate::RpcHost;

use super::napi::create_done_callback;
use super::rpc_host_message::RpcHostMessage;
use super::RpcConnectionNodejs;
use super::RpcConnectionNodejsMulti;

// RpcHostNodejs has a connection to the main Nodejs thread and manages
// the lazy initialization of Nodejs worker threads.
pub struct RpcHostNodejs {
  threadsafe_function: ThreadsafeFunction<RpcHostMessage>,
  node_workers: u32,
}

impl RpcHostNodejs {
  pub fn new(env: &Env, callback: JsFunction, node_workers: u32) -> napi::Result<Self> {
    // Create a threadsafe function that casts the incoming message data to something
    // accessible in JavaScript. The function accepts a return value from a JS callback
    let mut threadsafe_function: ThreadsafeFunction<RpcHostMessage> = env
      .create_threadsafe_function(
        &callback,
        0,
        |ctx: ThreadSafeCallContext<RpcHostMessage>| {
          let id = ctx.env.create_uint32(ctx.value.get_id())?.into_unknown();
          let (message, callback) = Self::map_rpc_message(&ctx.env, ctx.value)?;
          Ok(vec![id, message, callback])
        },
      )?;

    // Normally, holding a threadsafe function tells Nodejs that an async action is
    // running and that the process should not exist until the reference is released (like an http server).
    // This tells Nodejs that it's okay to terminate the process despite active reference.
    threadsafe_function.unref(&env)?;

    Ok(Self {
      node_workers,
      threadsafe_function,
    })
  }

  fn call_rpc(&self, msg: RpcHostMessage) {
    if !matches!(
      self
        .threadsafe_function
        .call(Ok(msg), ThreadsafeFunctionCallMode::NonBlocking),
      Status::Ok
    ) {
      return;
    };
  }

  // Map the RPC message to a JavaScript type
  fn map_rpc_message(env: &Env, msg: RpcHostMessage) -> napi::Result<(JsUnknown, JsUnknown)> {
    Ok(match msg {
      RpcHostMessage::Ping { response: reply } => {
        let message = env.to_js_value(&())?;
        let callback = create_done_callback(&env, reply)?;
        (message, callback)
      }
    })
  }
}

// Forward events to Nodejs
impl RpcHost for RpcHostNodejs {
  fn ping(&self) -> anyhow::Result<()> {
    let (tx, rx) = channel();
    self.call_rpc(RpcHostMessage::Ping { response: tx });
    Ok(rx.recv()?.map_err(|e| anyhow::anyhow!(e))?)
  }

  fn start(&self) -> anyhow::Result<RpcConnectionRef> {
    let mut connections = vec![];

    for _ in 0..self.node_workers {
      connections.push(RpcConnectionNodejs::new())
    }

    Ok(Arc::new(RpcConnectionNodejsMulti::new(connections)))
  }
}
