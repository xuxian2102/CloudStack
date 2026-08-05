use cloudstack_core::AppError;

/// 在 GIO 共享 I/O 线程池执行阻塞领域操作，再把结果送回 GTK 主线程。
/// GTK/GObject 绝不跨线程移动；后台闭包只能捕获 Send 的纯 Rust 数据。
pub fn run<T, Work, Complete>(work: Work, complete: Complete)
where
    T: Send + 'static,
    Work: FnOnce() -> Result<T, AppError> + Send + 'static,
    Complete: FnOnce(Result<T, AppError>) + 'static,
{
    gtk::glib::spawn_future_local(async move {
        let result = match gtk::gio::spawn_blocking(work).await {
            Ok(result) => result,
            Err(_) => Err(AppError::Io("后台文件任务异常终止".into())),
        };
        complete(result);
    });
}
