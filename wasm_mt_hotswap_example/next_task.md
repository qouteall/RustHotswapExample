# Next task

Implement a thread pool using the web worker manager.

Change web worker manager's initialization function to return a Vec of thread ids (excluding main thread).

The new thread pool's `new` accepts a list of thread ids (must not include main thread).

The requirement is that it can handle tasks with JS payloads. But not all messages have JS payloads (only a small amount has). Handling JS payload need to send web worker message, which involve wasm/JS copying and context switch etc. so it's slow. So the pure-Rust tasks should be handled without web worker message. 

Due to that requirement, it's not trivial. It's different to native thread pool.

It handles pure-Rust tasks via a scheduler loop. Each thread has a fixed-sized ring buffer queue. The thread pool has a shared deque that's protected by lock (plan to make it lock-free). The tasks that are submitted in thread in pool goes into current thread's ring buffer. If ring buffer is full it goes into shared deque. The tasks sent by other threads directly go to shared deque.

The in-memory queue only holds pure-Rust messages. The JS messages are sent via web worker message. The two are separate.

Each thread also has a global atomic flag that tells whether it should exit scheduler loop to handle JS web worker messages. (JS message cannot be handled without exiting current message handling) During the JS web worker message handling, once it finishes that task, it goes into scheduler loop again. JS tasks have higher priority so that scheduler loop exits when there are still remaining Rust task.

The scheduler loop sleeps using WASM mechanism. (TODO how exactly? std::thread::park?)

No need to consider async for now. Make it normal thread pool that runs blocking tasks.

By utilizing web worker manager, that thread pool should be able to be implemented by Rust without changing JS.

## Infrastructure

It requires some infrastructure

- Lock-free ring buffer queue. Can an existing crate be used? It needs to support no-std.
- Sleep using WASM mechanism. Does it require CondVar? We already have web-safe mutex. Do we need to make a web-safe CondVar?

## Race condition

It's complex. It may involve race conditions that make it deadlock or malfunction. Should be very careful.

(There is loom https://github.com/tokio-rs/loom testing tool but it uses std::thread which is not supported in wasm, unfortunately)

