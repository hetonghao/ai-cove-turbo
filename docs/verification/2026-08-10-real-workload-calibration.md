# Turbo 真实使用分布校准

这层校准复用现有四路公网 benchmark，不连接生产数据库，也不会自动修改 `src/telemetry.js`。生产侧只负责导出匿名聚合 profile；Turbo benchmark 负责把历史分布与本次传输机制样本合并成候选常量报告。

## 运行边界

- 公网模型 benchmark 会产生付费请求，只有获得明确授权并注入 `AI_COVE_API_KEY` 后才能运行。
- profile 不得包含用户 ID、提示词、IP、Token 名称、请求 ID、API Key 或完整请求头；解析器拒绝未声明字段。
- `measurement_date` 的 `anchor_date` 必须是 D-15，且历史至少三天。少于五天时，每个缺失日期必须使用固定原因码说明。
- 每条核心路径必须有至少 8 个无重试样本；HTTP 与 Hybrid Proxy 在整次运行中常驻复用，贴近桌面 Turbo 的连接生命周期。
- Hybrid 先单独记录一次冷启动请求并等待连接池明确出现空闲预热连接；这条冷启动证据不进入候选常量。正式 Hybrid 样本必须全部走已预热 WS，且期间不得发生重连。
- 任一日期的同窗或全天匹配覆盖率低于 70%，或历史方向与机制层相反时，不生成候选常量。
- 首输出固定收益使用 HTTP 与 Hybrid 的同轮配对差；至少 8 对无重试样本，且前后两半都必须保持正收益。前后半漂移进入报告，但不再用固定 15% 阈值否决方向一致的收益。
- 首输出与 complete 分别把 baseline 与 saved 作为一个收益常量组：只有新组计算出的提速比例高于旧组时才更新；比例降低或未保持正收益时保留整组旧值，并继续披露本轮机制测量，不阻止其他候选生成。

## Profile 口径

可复制的完整脱敏示例见 [2026-08-10-workload-profile.example.json](./2026-08-10-workload-profile.example.json)。

`filters` 固定模型、渠道、`/v1/responses` 和流式状态。`bucket` 的四个键必须全部相同才算匹配：

- `input`：由当天 Turbo 输入 token 分位数冻结出的桶序号。
- `output`：边界固定为 `100 / 300 / 1000 / 3000`，对应五档输出。
- `cached_ratio`：由 profile 声明的缓存占比边界产生的桶序号。
- `reasoning_effort`：精确值；未知值必须单列，不能与其他档合并。

`current` 还需提供 HTTP/WS 请求数、raw/sent bytes 的 P50/P90，以及首事件和 complete 的 P50/P90。每个历史日期同时提供 `same_window` 与 `full_day`。历史桶少于 12 条或缺少 P90 时，报告保留 P50、样本数和覆盖率，但不构造 P90。

多桶统计先按当天分布标准化每个历史日，再对日期取中位数，因此每个日期等权，不按历史请求量加权。历史 before/after 只用于真实体验交叉验证；WebSocket 固定节省毫秒只来自常驻 Turbo HTTP 与已预热 Hybrid 纯 WS rounds 的差值。上传压缩收益单独按 5、10、20 Mbps 输出，避免重复计入 WS 固定收益。

如果精确时钟同窗因当天 `reasoning_effort` 分布变化而无法达到覆盖门槛，profile 可以把 `same_window` 明确标记为“全天同分布匹配”。这种情况下 `same_window` 与 `full_day` 数值相同，报告必须披露该降级口径，不能把它描述为精确时钟同窗。

候选报告在每个历史日的同窗和全天范围下逐桶输出当前样本数、历史样本数和 `profileCoveragePct`；该值是该匹配桶对当前 profile 总覆盖率的贡献，同一范围内求和等于逐日总覆盖率。

## 执行

先用 6 个逻辑请求验证常驻 Hybrid 的“冷启动一次 + 池 ready + 5 轮 warm WS continuation”，不生成常量：

```bash
export AI_COVE_API_KEY
npm run benchmark:smoke
```

烟测通过后再运行正式四路采样：

```bash
cd /Users/hetonghao/dev/hth-project/ai-cove/turbo

export AI_COVE_API_KEY
export TURBO_BENCHMARK_WORKLOAD_PROFILE=/absolute/path/to/anonymized-workload-profile.json
export TURBO_BENCHMARK_CANDIDATE_OUTPUT=/absolute/path/to/candidate-constants.json
export TURBO_BENCHMARK_RUNS=8
export TURBO_BENCHMARK_WARMUPS=1
export TURBO_BENCHMARK_MODEL=gpt-5.6-luna

npm run benchmark:live
```

终端输出包含四路 `median[min,max]`、逐样本传输分类、逐日历史校准和旧值/候选值变化。`TURBO_BENCHMARK_CANDIDATE_OUTPUT` 是机器可读 JSON；状态固定为 `candidate_not_applied`，需要人工审阅后另行决定是否更新产品常量。

## 2026-08-13 实测结果

脱敏机器证据见 [2026-08-13-turbo-speed-candidate.json](./2026-08-13-turbo-speed-candidate.json)。它保留 profile 指纹、样本量、覆盖率、四个旧/新常量和机制/历史分层指标，不包含用户 ID、提示词、请求头或密钥。

本次使用 `gpt-5.6-sol`、8 个正式样本、1 次 warmup。四条 continuation 核心路径均为 `8/8` 无重试；Hybrid 正式样本全部为 `WS→WS→WS→WS→WS`，零重连。冷启动的一次 HTTP 只用于确认连接池 ready，不进入候选常量。

真实 profile 为 HTH 当日 824 条有效样本（HTTP 1 / WS 823），历史日期为 2026-07-29 至 2026-08-02。由于精确时钟同窗不能覆盖当天 `reasoning_effort` 结构，本次 `same_window` 明确使用全天同分布匹配，与 `full_day` 相同；逐日覆盖率为 86.9%、99.6%、86.9%、87.5%、87.3%。

| 常量 | 旧值 | 新值 | 变化 | 含义 |
|---|---:|---:|---:|---|
| `baselineFirstTokenMs` | 1661 | 1669 | +8 ms / +0.5% | 五个历史日按真实请求分布标准化后的首输出 P50 日期中位数 |
| `baselineCompleteMs` | 2273 | 2273 | 0 ms / 0.0% | 新测历史 complete 基线为 11184 ms，但本轮 complete 机制收益未通过，按成组只增不减原则保留旧 baseline |
| `websocketFirstTokenSavedMs` | 469 | 521 | +52 ms / +11.1% | HTTP 与已预热 Hybrid 的同轮配对首输出节省；前后半漂移 3.0% |
| `websocketCompleteSavedMs` | 274 | 274 | 0 ms / 0.0% | 本轮 complete 配对差为 -399 ms，未证明更高收益，与旧 baseline 成组保留 |

机制层 continuation 聚合中位数：HTTP/Hybrid 首输出为 1197/608 ms，complete 为 1791/2439 ms。首输出同轮配对节省为 521 ms，因此首输出常量组更新为 1669/521；complete 同轮配对差为 -399 ms，因此 complete 常量组保留 2273/274。

真实日志交叉验证：当前首输出 P50 为 211 ms，历史标准化基线为 1669 ms，粗略提升 87.4%；当前 complete P50 为 8488 ms，历史标准化基线为 11184 ms，粗略提升 24.1%。这部分包含模型服务负载、缓存、服务等级和时段等因素，只能说明体验方向，不能全部归因于 Turbo。

按冻结画像的真实流量压缩分布，10 Mbps 下平均上传节省约 10.9 ms；结合 WS 请求占比，首输出估算收益为 31.8%，complete 估算收益为 12.5%。面板按实时压缩字节变化，先前同一批页面流量使用旧 complete 常量组时约为 14.4%～14.5%；本轮负向噪声不会把该产品收益档位下调。
