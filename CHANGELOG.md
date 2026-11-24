# 更新日志

本项目所有显著变更记录于此文件。
格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

### 新增（MVP，2026-11-23）

- 图片上传：支持 PNG / JPEG / GIF / WebP；动图保留原件，静图重编码去元数据
- blake3 内容哈希去重，相同文件不重复占用存储
- WebP 缩略图生成，图库浏览与删除
- 访问令牌鉴权：未设置 `AMOR_TOKEN` 即开放模式，已登录方可上传与管理
- 站点设置：标题、明暗主题、单文件 / 请求总量大小上限、favicon
- 一键复制 链接 / Markdown / HTML / BBCode 四种外链格式
- compose 单文件部署，镜像 `ghcr.io/vansour/amorimage`

### 安全（代码审查修复，2026-11-24）

- 上传改为流式读取、边读边检查限额，超限立即中止，不再先整个读入内存（修复 OOM 面）
- GIF 不再逐帧解码，杜绝恶意帧描述符声明的超大尺寸放大内存、导致进程崩溃
- 访问令牌改由 `.env` 注入，不再硬编码在 compose.yaml（参考 `.env.example`）
- 鉴权按真实路由形状 + 方法放行，未认证请求不再能拿到完整前端壳

### 修复（代码审查修复，2026-11-24）

- 灰度与 16 位图片可正常上传（此前缩略图 WebP 编码失败导致整体被拒）
- 静态 GIF 可正常上传（此前落入重编码分支被拒）
- 2MB–10MB 的 favicon 可正常上传（16MB 传输限额挂到正确路由）
- 未配置 `AMOR_BASE_URL` 时，Markdown / HTML / BBCode 复制不再出现 `undefined` 链接
- 容器内响应 SIGTERM，优雅停机生效
- 大小上限设置防 u64 溢出，超范围（>1TB）直接拒绝并提示
- `AMOR_TOKEN` 含非法字符（非 ASCII 或分号）时启动即报错，不再登录 500 / 无限循环
- 图片解码、重编码移入阻塞线程池，并发上传不再拖死整个服务
- favicon 缓存缩短为 5 分钟并要求重新验证，更换后浏览器标签页及时更新
- 构建上下文不再携带 target/、node_modules/ 与活动数据库（新增 `.dockerignore`）
- 全新克隆可直接 `cargo build`（构建脚本自动补建空的 `frontend/dist`，缺前端时运行提示先构建）
