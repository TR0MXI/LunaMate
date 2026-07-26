//! 数据库 façade 与内存后端测试。

mod atomic_file;
mod engine;
mod storage;

use std::future::Future;

fn run_async<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试必须能创建 Tokio 运行时")
        .block_on(future)
}
