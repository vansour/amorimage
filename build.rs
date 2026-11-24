fn main() {
    // RustEmbed 在目录缺失时于编译期 panic；全新克隆还没有 frontend/dist，
    // 构建脚本先于宏展开运行，这里补建空目录（运行时由 serve_static 提示先构建前端）
    std::fs::create_dir_all("frontend/dist").expect("创建 frontend/dist 失败");
}
