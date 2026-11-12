//! 项目参数, 来自仓库根目录的 params.json, 编译时嵌入 exe.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Params {
    pub update: Update,
    pub panel: Panel,
    pub predict: Predict,
    pub learn: Learn,
    pub jev: Jev,
    pub timeline: Timeline,
    pub history: History,
    pub redact: Redact,
    /// 与 installer/qrun.iss 的 AppId 一致, 用来找安装包写入的卸载信息.
    pub installer_app_id: String,
}

#[derive(Deserialize)]
pub struct Update {
    /// 发行版地址, 按顺序尝试: 前面的取不到再用后面的.
    pub sources: Vec<String>,
    pub check_interval_hours: u64,
    pub timeout_secs: u32,
}

#[derive(Deserialize)]
pub struct Panel {
    /// 开了悬停切换分组时, 鼠标在标签上停多久才切, 划过标签栏时不会连着切.
    pub hover_switch_delay_ms: u64,
    /// 拖出面板时跟着光标的图标尺寸 (物理像素), 与资源管理器拖文件时的缩略图相当.
    pub drag_image_size: i32,
}

/// 预测下一个要打开的东西 (model::predict).
#[derive(Deserialize)]
pub struct Predict {
    /// 候选的半衰期 (天), 与 hour_sigma 两两组合, 回放历史选命中率最高的一组.
    pub half_life_days: Vec<f64>,
    /// 候选的钟点摊开宽度 (小时): 一次启动按正态核摊到前后钟点上.
    pub hour_sigma: Vec<f64>,
    /// 钟点, 工作日/周末, 前后衔接三项向整体分布平滑的强度 (相当于几次虚拟启动).
    pub smoothing: f64,
    /// 没启动过的候选的虚拟次数, 让它们也有分数, 排在启动过的后面.
    pub prior_count: f64,
    /// 距上次启动超过这么久就算重新开始干活, 不算"接着开".
    pub session_gap_minutes: i64,
    /// 系统记录里的启动和 QRun 自己记的同一目标相差不到这么久, 算同一次, 不重复记.
    pub dedupe_seconds: i64,
    /// 系统记录里文件夹和它里面的文件相差不到这么久出现, 是打开文件时系统顺带记的, 文件夹不算一次启动.
    pub folder_merge_seconds: i64,
    /// 回放最近多少天的启动来调参.
    pub replay_days: i64,
    /// 回放时进前几名算命中.
    pub replay_top_k: usize,
    /// 每隔多久重新回放调参一次.
    pub replay_interval_hours: u64,
}

/// 从每次打开时的对错里学特征权重 (model::predict::Weights).
#[derive(Deserialize)]
pub struct Learn {
    /// AdaGrad 的基础步长: 每个特征的第一步约这么大, 之后随累计梯度变小.
    pub rate: f64,
    /// 往起步权重回拉的强度, 算在每次学习的梯度里: 权重偏离起步值 1 时, 回拉相当于 l2 那么大的反向梯度.
    pub l2: f64,
    /// Jev 一项的起步权重和学习时的下限.
    pub jev_initial: f64,
    pub jev_floor: f64,
    /// 记录类特征 (常用度, 钟点, 前后衔接, 情境等, 见 model::predict::SKIPPED 之前) 权重学习时的下限.
    pub evidence_floor: f64,
    /// Jev 特征取 ln(p + 这个值): p 为 0 时不至于一票否决.
    pub jev_smoothing: f64,
    /// 除前四项和 Jev 外, 不从 0 起步的特征 (特征名见 model::predict::FEATURES).
    pub initial: HashMap<String, f64>,
    /// 在"猜你要打开"里点了某个时, 只有它上面最近的这么多个算被跳过.
    pub skip_window: usize,
    /// 被跳过的离被点的每远一位, 反例分量乘这个数.
    pub skip_decay: f64,
    /// 在分组或搜索里另点了别的时, "猜你要打开"的前这么多个算被跳过.
    pub elsewhere_skipped: usize,
    /// 设置里显示最近多少天的命中率.
    pub hit_rate_days: i64,
    /// "最近被跳过几次"往回看多少天.
    pub skip_days: i64,
    /// 下载文件夹和桌面上多少天内的文件算新文件.
    pub new_file_days: i64,
    /// 两次激活面板相隔不到这么久算同一次打开 (贴边滑出和激活窗口常紧挨着发生).
    pub open_merge_seconds: u64,
    /// 没换窗口, 没出现新文件, 也没点过东西时, 这么久内再打开沿用上次的排序.
    pub keep_order_minutes: i64,
}

/// Jev 决策模型 (Decisions API).
#[derive(Deserialize)]
pub struct Jev {
    /// 设置里接口地址的默认值 (TypeSafe 官方); 请求发到 `{接口地址}/systemone`.
    pub base_url: String,
    pub model: String,
    pub timeout_secs: u32,
    /// 一次最多让 Jev 在多少个候选里选 (接口上限 255).
    pub max_candidates: usize,
    /// state 序列化后的字节上限; 超过约 32000 字节 Jev 会切到更慢更不准的长上下文模式.
    pub max_state_bytes: usize,
    /// 带上最近多少次启动.
    pub history: usize,
    /// 切到一个窗口 (或窗口换了标题) 后停这么久才预取, 快速切换时不调.
    pub prefetch_delay_ms: u64,
    /// 前台窗口和标题都没变时每隔多久重新预取.
    pub refresh_minutes: u64,
    /// 两次预取至少隔多久; 标题一直在变的窗口 (进度, 计时) 不会连着调.
    pub prefetch_min_interval_seconds: u64,
    /// 多久没有键盘鼠标输入算人离开了, 离开时不预取.
    pub idle_minutes: u64,
    /// 预取结果超过多久算过旧, 打开面板时要补调.
    pub stale_minutes: u64,
    /// 打开面板后多久内 (且鼠标还没进推荐页) 允许按补调的结果重排, 之后顺序锁定到下次打开.
    pub reorder_window_ms: u64,
    /// 搜索框停止输入多久才调.
    pub search_debounce_ms: u64,
    /// 搜索时名称不匹配的候选, Jev 给的概率至少这么高才显示.
    pub search_min_probability: f64,
}

/// 活动时间线 (model::library::timeline 合并和存放, app::sensors 采集). 人离开的判断沿用 jev.idle_minutes.
#[derive(Deserialize)]
pub struct Timeline {
    /// 内存里留最近多少分钟.
    pub keep_minutes: i64,
    /// 最多留多少条, 超过丢最早的.
    pub max_events: usize,
    /// 每隔多久写一次库 (退出时也写), 崩溃最多丢这么久.
    pub save_minutes: u64,
    /// 每次打开 QRun 时随那次打开存下最近多少条.
    pub summary_events: usize,
    /// 前台窗口停留不到这么久算切换途中经过, 换走时删掉.
    pub focus_min_seconds: i64,
    /// 同一窗口, 剪贴板相隔不到这么久的合并成一条.
    pub merge_seconds: i64,
    /// 同一文件一直在改 (相邻两次相隔不到这么久) 就延长同一条, 后台程序不停写的日志不会挤满时间线.
    pub file_merge_minutes: i64,
    /// 每隔多久看一次程序启停.
    pub process_interval_seconds: u64,
    /// 每隔多久看一次监听端口.
    pub port_interval_seconds: u64,
    /// 同一目录 (监听目录下第一级) burst_window_seconds 内改动超过 burst_events 次时, 之后的合并成一条 (编译, 解压, 同步).
    pub burst_window_seconds: i64,
    pub burst_events: usize,
    /// 最近活动对推荐的影响每过这么久减半 (被改过, 在前台, 刚启动, 路径相近).
    pub activity_half_life_minutes: i64,
    /// 时间线里碰过的目标最多取多少个当候选.
    pub candidates: usize,
    /// 算"和最近活动的路径有多近"时最多比较多少条路径.
    pub nearby_paths: usize,
    /// 打开 QRun 前多少分钟内发生的事, 算作"出现这些事之后打开了什么".
    pub token_minutes: i64,
    /// "出现某件事之后打开了什么"的统计每过这么多天减半.
    pub cooccur_half_life_days: i64,
    /// 给 Jev 看最近多少条活动.
    pub jev_events: usize,
}

/// 长期保留的活动记录 (model::library::timeline 归档进 history), 回放用.
#[derive(Deserialize)]
pub struct History {
    /// 结束超过这么久的事件才归档: 要长于前台心跳合并, 同一文件连着改的合并, 人离开时往回截的范围, 归档后不会再变.
    pub archive_after_minutes: i64,
    /// 设置里 "保留天数" 的默认值.
    pub retention_days: u32,
}

/// 脱敏 (model::redact) 和剪贴板读取.
#[derive(Deserialize)]
pub struct Redact {
    /// 剪贴板文字只读开头这么多个字符; 看开头就够判断是什么, 也少碰内容.
    pub clip_chars: usize,
    /// 跟在这些字样后 (用 = 或 : 隔开) 的值一律换掉, 拦住不随机的弱密码; 不分大小写.
    pub keywords: Vec<String>,
    /// 至少这么长的串才可能被当成随机串 (密钥, 令牌, 哈希).
    pub min_secret_len: usize,
    /// 随机串里字母和数字至少交替这么多次; 驼峰标识符和单词连写交替很少, 不会被误伤.
    pub min_digit_switches: usize,
    /// 随机串的香农熵至少达到这么长的串理论上限的这个比例.
    pub entropy_ratio: f64,
}

pub static PARAMS: LazyLock<Params> =
    LazyLock::new(|| serde_json::from_str(include_str!("../params.json")).expect("params.json"));
