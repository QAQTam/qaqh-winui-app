use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use markdown_winui::{BlockTranscript, BlockTurnView};
use qaqh_fluent::{motion, tokens};
use windows_reactor::*;

use crate::bridge::Bridge;
use crate::chat_adapter;

use super::cache::{cache_store, cache_take};
use super::tools::log_diag;
use super::turns::{TurnProps, turn_memo};
use super::*;

// Phase 2：timeline 单源后，conversation 事件的跨帧合并（merge_deferred_event/
// same_stream_target）随 chat_events 队列退役（M3 双发→timeline 单源），
// 见 TIMELINE-MIGRATION-ROADMAP Phase 3。
pub fn chat_view(cx: &mut RenderCx, bridge: Arc<Bridge>) -> Element {
    let color_scheme = cx.use_color_scheme();
    let transcript = cx.use_ref::<BlockTranscript>(BlockTranscript::new());
    let frame_revoker = cx.use_ref::<Option<EventRevoker>>(None);
    let last_rev = cx.use_ref::<u64>(0);
    let last_seed = cx.use_ref::<String>(String::new());
    // 显式跟随状态机：按钮/会话切换进入，on_view_changed 离开驱动
    let follow_state = cx.use_ref::<FollowState>(FollowState::Following);
    // 数据规模诊断日志节流基准（5s；见 timer 泵内的 scale 快照）
    let last_scale_log = cx.use_ref::<std::time::Instant>(std::time::Instant::now());
    // 已成功 restore 的 seed：空态文案区分"快照已加载但会话为空"（全新
    // 空会话 → "开始新的对话…"）与"快照未到达仍在加载"（→ "加载会话…"）。
    let last_restored_seed = cx.use_ref::<String>(String::new());
    // BUG-F1：空快照核实轮数（见 EMPTY_SNAPSHOT_VERIFY_MAX）。每次消费到
    // n==0 快照时递增并重拉核实；n>0 恢复或种子切换时归零。
    let empty_snapshot_verifications = cx.use_ref::<u32>(0);
    // 跟随尾部滚动请求版本：pump 内容变化时递增（restore/新 turn 立即、
    // live 增量节流），render 时随 list_view.follow_tail 下发
    // ——reconciler 检测版本变化后按 near-tail 判定执行贴底滚动
    // （用户离开底部时不打扰）。
    let scroll_version = cx.use_ref::<u64>(0);
    // restore 是唯一需要无条件贴底的路径；普通增量必须尊重用户上滚。
    let force_tail_version = cx.use_ref::<Option<u64>>(None);
    // 滚动请求节流基准（live 流式限频，见 SCROLL_REQUEST_THROTTLE）。
    let last_scroll_request = cx.use_ref::<std::time::Instant>(std::time::Instant::now());
    // deferred（快照 seed 不匹配）日志限频：vsync 泵每帧都会命中，
    // 不节流会刷爆日志（spawn_timeline_refresh 本身有 1s 节流）。
    // UI 提交代次与 transport rev 解耦：seed、快照、分页和事件批次都可
    // 独立提交，不会因构造 rev 与下一条传输 rev 碰撞而漏帧。
    let render_generation = cx.use_ref::<u64>(0);
    let (_, set_render_generation) = cx.use_state::<u64>(0);
    // Live 正文提交节流基准；Structural 变更立即提交。
    let last_live_render = cx.use_ref::<std::time::Instant>(std::time::Instant::now());
    // 跨帧 reducer 余量：bridge 的数量限额先取一批，再按 UI 时间预算
    // 分段应用；未处理事件保留在此，下一次 CompositionTarget 帧继续。
    // Phase 2：timeline entry（全局单调 seq，无需 coalesce）。
    let deferred_events = cx.use_ref::<VecDeque<markdown_winui::TimelineEntry>>(VecDeque::new());
    // 后台恢复检测：上一次 vsync 帧回调时刻。窗口最小化/不可见时合成器
    // 暂停回调，恢复可见后首帧 elapsed 即后台时长——超过阈值走"快照
    // 恢复"（丢弃积压增量 + 拉 timeline 快照一次到位），而非重放后台
    // 期间积压的几千条增量事件。vsync 未停（可见）则增量正常消费，
    // 此检测永不触发——逻辑自洽。
    let last_frame_at = cx.use_ref::<std::time::Instant>(std::time::Instant::now());
    // 本"可见期"是否已做过后台恢复（每帧检测，只处理一次）。
    let background_resumed = cx.use_ref::<bool>(false);
    // 后台恢复进行中（`Some(seed)` = 快照已重拉、尚未到达）：渲染
    // shimmer 加载覆盖层盖住内容（不白屏）；快照 restore 后清除。
    let recovering = cx.use_ref::<Option<String>>(None);
    // 覆盖层 shimmer 光带相位（90ms 步进）。
    let shimmer_tick = cx.use_ref::<u64>(0);
    let last_shimmer_step = cx.use_ref::<std::time::Instant>(std::time::Instant::now());
    // 锚定补偿挂起标记：`Some(rows)` = 本帧渲染需用 within 滚动（顶部
    // 预加载后把「原窗口首行」锚回原位，视口不跳）。rows = 扩展前移量
    // = 原首行的新下标。渲染闭包 take 后随 preserve_anchor 下发。
    let pending_anchor = cx.use_ref::<Option<usize>>(None);
    // 窗口快照缓存：按 Transcript 投影 rev + theme 复用整包；有变化时
    // 按 turn_id + turn mutation_rev 结构共享未变化行。主题纳入缓存键，
    // 避免 ActualThemeChanged 时仍把旧主题的 realized 行当作同一数据包。
    let window_cache = cx.use_ref::<Option<((u64, ColorScheme), Rc<Vec<Rc<BlockTurnView>>>)>>(None);
    // transport drain 快速闸：8ms 下限，随 vsync 逐帧消费（见 DRAIN_INTERVAL）。
    let last_drain = cx.use_ref::<Option<std::time::Instant>>(None);

    // 事件泵：drain bridge 队列 → Transcript；rev 变化触发重渲染。
    // with_cleanup：卸载时把当前投影移入 bridge 级 transcript 缓存（Fix B）。
    cx.use_effect_with_cleanup((), {
        let bridge = bridge.clone();
        let transcript = transcript.clone();
        let frame_revoker = frame_revoker.clone();
        let last_rev = last_rev.clone();
        let last_seed = last_seed.clone();
        let empty_snapshot_verifications = empty_snapshot_verifications.clone();
        let last_restored_seed = last_restored_seed.clone();
        let last_scroll_request = last_scroll_request.clone();
        let scroll_version = scroll_version.clone();
        let force_tail_version = force_tail_version.clone();
        let follow_state = follow_state.clone();
        let last_scale_log = last_scale_log.clone();
        let pending_anchor = pending_anchor.clone();
        let render_generation = render_generation.clone();
        let set_render_generation = set_render_generation.clone();
        let last_drain = last_drain.clone();
        let last_live_render = last_live_render.clone();
        let deferred_events = deferred_events.clone();
        let last_frame_at = last_frame_at.clone();
        let background_resumed = background_resumed.clone();
        let recovering = recovering.clone();
        let shimmer_tick = shimmer_tick.clone();
        let last_shimmer_step = last_shimmer_step.clone();
        move || -> Option<Box<dyn FnOnce()>> {
            if frame_revoker.borrow().is_some() {
                return None;
            }
            match on_frame({
                let bridge = bridge.clone();
                let transcript = transcript.clone();
                let last_rev = last_rev.clone();
                let last_seed = last_seed.clone();
                let empty_snapshot_verifications = empty_snapshot_verifications.clone();
                let last_restored_seed = last_restored_seed.clone();
                let last_scroll_request = last_scroll_request.clone();
                let scroll_version = scroll_version.clone();
                let force_tail_version = force_tail_version.clone();
                let render_generation = render_generation.clone();
                let set_render_generation = set_render_generation.clone();
                let last_live_render = last_live_render.clone();
                let deferred_events = deferred_events.clone();
                let last_frame_at = last_frame_at.clone();
                let background_resumed = background_resumed.clone();
                let recovering = recovering.clone();
                let shimmer_tick = shimmer_tick.clone();
                let last_shimmer_step = last_shimmer_step.clone();
                let pending_anchor = pending_anchor.clone();
                move || {
                    // panic 防护：任何 RefCell 冲突/索引越界等 panic 只记日志，
                    // 绝不穿过 WinUI FFI 边界（否则 = stowed exception
                    // 0xc000027b 进程崩溃——2026-08-15 会话切换崩溃的止血）。
                    let panic_result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // 后台恢复检测：窗口不可见时 vsync 回调暂停，恢复可见
                    // 后首帧 elapsed = 后台时长。超过阈值 → 丢弃积压增量
                    // （快照会覆盖 transcript）+ 清 last_restored_seed 触发
                    // 快照重拉 → 复用 restore 分支一次到位（毫秒级）。
                    // 同时点亮 shimmer 覆盖层（快照到达 restore 后熄灭）。
                    let now = std::time::Instant::now();
                    let frame_interval = now.duration_since(*last_frame_at.borrow());
                    if frame_interval >= BACKGROUND_RESUME_AFTER
                        && !*background_resumed.borrow()
                    {
                        *background_resumed.borrow_mut() = true;
                        // 丢弃积压 timeline 增量（快照覆盖兜底）。
                        bridge.core().timeline_drain_limit(usize::MAX);
                        deferred_events.borrow_mut().clear();
                        *last_restored_seed.borrow_mut() = String::new();
                        *recovering.borrow_mut() = Some(bridge.core().active_seed());
                        log_diag("chat_view: background resume (drop backlog + snapshot refresh)");
                    }
                    *last_frame_at.borrow_mut() = now;
                    // 诊断：帧间隔（>2s = 暂停恢复，diagnostics 内过滤）。
                    crate::diagnostics::record_frame_interval(frame_interval.as_secs_f64() * 1000.0);
                    // QAQ-Harness perf diagnostic: data-scale snapshot every 5s
                    // (turns in memory vs windowed render rows vs viewports).
                    if now.duration_since(*last_scale_log.borrow())
                        >= std::time::Duration::from_secs(5)
                    {
                        *last_scale_log.borrow_mut() = now;
                        let t = transcript.borrow();
                        log_diag(&format!(
                            "chat_view scale: turns={} window={} rev={}",
                            t.turns().len(),
                            t.window_len(),
                            t.mutation_rev(),
                        ));
                        // 诊断：数据规模快照（与 log_diag 同频）。
                        crate::diagnostics::record_scale(
                            t.turns().len() as u32,
                            t.window_len() as u32,
                            t.mutation_rev(),
                        );
                    }
                    // 会话切换：缓存旧会话的原生投影，立即恢复目标会话；
                    // canonical 快照仍会异步刷新并覆盖缓存。
                    let seed = bridge.core().active_seed();
                    if seed != *last_seed.borrow() {
                        let previous_seed = last_seed.borrow().clone();
                        if !previous_seed.is_empty() {
                            // BUG-003 根因 2：存缓存 mem::take（旧投影整体移入，
                            // 不再全量 clone）；restore 侧 entries.remove 对称零拷贝。
                            let mut cached = std::mem::take(&mut *transcript.borrow_mut());
                            cached.trim_to_window();
                            cache_store(previous_seed, cached);
                        }
                        *last_seed.borrow_mut() = seed.clone();
                        // UI 侧尚未归并的旧会话事件不能跨 seed 留存。bridge
                        // 队列有 seed 过滤，但本地 deferred 队列也必须清空。
                        deferred_events.borrow_mut().clear();
                        *empty_snapshot_verifications.borrow_mut() = 0;
                        let cached = cache_take(&seed);
                        let had_cached = cached.is_some();
                        *transcript.borrow_mut() = cached.unwrap_or_else(BlockTranscript::new);
                        *last_restored_seed.borrow_mut() = if had_cached {
                            seed.clone()
                        } else {
                            String::new()
                        };
                        *last_rev.borrow_mut() = 0;
                        *scroll_version.borrow_mut() += 1;
                        let generation = *scroll_version.borrow();
                        // BUG-004 决策：恢复会话 = 回到最新底部，不恢复上次浏览位置。
                        *force_tail_version.borrow_mut() = Some(generation);
                        *follow_state.borrow_mut() = FollowState::Following;
                        *render_generation.borrow_mut() += 1;
                        set_render_generation.call(*render_generation.borrow());
                        if !seed.is_empty() {
                            bridge.core().spawn_timeline_refresh(&seed);
                        }
                        log_diag(&format!("chat_view: switched to seed {seed}"));
                    }
                    // drain 快速路径：8ms 下限在 60Hz/120Hz 屏上逐帧放行，
                    // 避免 32ms 攒批造成可见的成批吐字。deferred 非空时仍
                    // 放行小批（/4），单帧总成本由事件上限和 reducer 4ms
                    // 预算兜底，结构事件也不会被 backlog 长时间阻塞。
                    let drain_gate = last_drain
                        .borrow()
                        .is_none_or(|t| now.duration_since(t) >= DRAIN_INTERVAL);
                    if drain_gate {
                        *last_drain.borrow_mut() = Some(now);
                    }
                    let fetch_limit = if !drain_gate && deferred_events.borrow().is_empty() {
                        0
                    } else if deferred_events.borrow().is_empty() {
                        CHAT_EVENTS_PER_FRAME
                    } else {
                        CHAT_EVENTS_PER_FRAME / 4
                    };
                    let (events, rev) = bridge.core().timeline_drain_limit(fetch_limit);
                    // 诊断：drain 吞吐（timeline 事件）。
                    crate::diagnostics::record_drain(events.len() as u32);
                    // 1) timeline 快照（resume 历史；peek + seed 校验）：
                    //    匹配才消费；不匹配**保留**快照（不丢弃）并主动重拉
                    //    active seed 的快照——原 take 语义消费即弃，丢弃后
                    //    daemon 不重推，快照永久丢失 → ChatView 永远停在
                    //    "加载会话…"。
                    if let Some(snap) = bridge.core().chat_timeline_take(&seed) {
                        let snap_view =
                            chat_adapter::timeline_snapshot(&snap).unwrap_or_default();
                        let n = snap_view.turns.len();
                        // BUG-F1：n==0 的空快照不得直接作为「已恢复」凭据——
                        // 单槽里可能驻留会话创建时期的过期空快照，直接采信会
                        // 覆盖真实内容并熔断重拉（重挂载后永久空白）。先核实：
                        // 保持加载态并重拉（1s 节流），EMPTY_SNAPSHOT_VERIFY_MAX
                        // 轮仍为空才采信为真空会话（恢复终态）。
                        let verified_empty = *empty_snapshot_verifications.borrow()
                            >= EMPTY_SNAPSHOT_VERIFY_MAX;
                        if n > 0 || verified_empty {
                            let mut transcript = transcript.borrow_mut();
                            transcript.restore(&snap_view);
                            drop(transcript);
                            *last_restored_seed.borrow_mut() = seed.clone();
                            *empty_snapshot_verifications.borrow_mut() = 0;
                            // BUG-004 决策：恢复 = 回到最新底部（force_tail），
                            // 不恢复上次浏览位置。
                            // 后台恢复完成：熄灭 shimmer 覆盖层（快照内容已
                            // 就位 + 贴底滚动在同一帧下发，无白屏窗口）。
                            if recovering.borrow().as_ref() == Some(&seed) {
                                *recovering.borrow_mut() = None;
                                log_diag("chat_view: recovery overlay dismissed");
                            }
                            let now = std::time::Instant::now();
                            *scroll_version.borrow_mut() += 1;
                            *force_tail_version.borrow_mut() = Some(*scroll_version.borrow());
                            *follow_state.borrow_mut() = FollowState::Following;
                            *last_scroll_request.borrow_mut() = now;
                            *render_generation.borrow_mut() += 1;
                            set_render_generation.call(*render_generation.borrow());
                            if n > 0 {
                                log_diag(&format!(
                                    "chat_view: restored {n} turns for {seed} (force_tail)"
                                ));
                            } else {
                                log_diag(&format!(
                                    "chat_view: empty snapshot accepted as terminal for {seed}"
                                ));
                            }
                        } else {
                            // 不覆盖既有 transcript（可能来自 Fix B 卸载缓存），
                            // 保持加载态，主动重拉核实。
                            *empty_snapshot_verifications.borrow_mut() += 1;
                            bridge.core().spawn_timeline_refresh(&seed);
                            log_diag(&format!(
                                "chat_view: empty snapshot held (verify {}/{}) for {seed}",
                                *empty_snapshot_verifications.borrow(),
                                EMPTY_SNAPSHOT_VERIFY_MAX
                            ));
                        }
                    } else {
                        // 快照缺失（activate_timeline 失败/未达/从未激活）：
                        // 主动重拉——否则冷启动/重建后 ChatView 永久停在
                        // "加载会话…"，只有发送消息（增量事件）才出现内容。
                        // 空会话 restore 后 last_restored_seed == seed，不再重拉。
                        if !seed.is_empty() && *last_restored_seed.borrow() != seed {
                            bridge.core().spawn_timeline_refresh(&seed);
                        }
                    }
                    // 1.5) 分页页（上滚翻页的更早回合）：drain → 前插 →
                    //     锚定补偿。用户上滚浏览中（tail_following=false），
                    //     前插后窗口起点已右移，用 pending_anchor 把
                    //     「原窗口首行」锚回原位，视口不跳。
                    let pages = bridge.core().chat_prepend_drain();
                    if !pages.is_empty() {
                        let mut t = transcript.borrow_mut();
                        let mut prepended = 0usize;
                        for (_, page) in pages {
                            let snap_view =
                                chat_adapter::timeline_snapshot(&page).unwrap_or_default();
                            prepended += t.prepend_snapshot(&snap_view);
                        }
                        if prepended > 0 {
                            *pending_anchor.borrow_mut() = Some(prepended);
                            *scroll_version.borrow_mut() += 1;
                            *render_generation.borrow_mut() += 1;
                            set_render_generation.call(*render_generation.borrow());
                            log_diag(&format!(
                                "chat_view: prepended {prepended} turns for {seed}"
                            ));
                        }
                    }
                    // 2) 增量事件（新对话流式）：timeline live entry → 跨帧
                    // 队列 → reducer 时间预算应用；积压显式保留，下一帧继续。
                    if rev != *last_rev.borrow() {
                        *last_rev.borrow_mut() = rev;
                    }
                    if !events.is_empty() {
                        let mapped = events
                            .iter()
                            .filter_map(chat_adapter::timeline_entry);
                        let mut deferred = deferred_events.borrow_mut();
                        deferred.extend(mapped);
                    }
                    let reducer_started = std::time::Instant::now();
                    let mut update = markdown_winui::TranscriptChange::default();
                    {
                        let mut t = transcript.borrow_mut();
                        while reducer_started.elapsed() < CHAT_REDUCER_BUDGET {
                            let Some(entry) = deferred_events.borrow_mut().pop_front() else {
                                break;
                            };
                            update.merge(t.apply_entry(&entry));
                        }
                    }
                    if update.changed() {
                        let structural = update.is_structural();
                        let now = std::time::Instant::now();
                        // 滚动只随真正改变内容 extent 的 XAML 提交发生；
                        // 状态徽标更新不再制造多余 ScrollViewer 请求。
                        if update.extent_changed
                            && (structural
                                || now.duration_since(*last_scroll_request.borrow())
                                    >= SCROLL_REQUEST_THROTTLE)
                        {
                            // 窗口跟随尾部：仅当窗口未被用户上滚扩展时滑动
                            // （保持最近 N 个回合，长会话不退化）；用户上滚
                            // 扩展后窗口保持，避免浏览内容跳动。
                            if structural && transcript.borrow().tail_following() {
                                transcript.borrow_mut().slide_window_tail();
                            }
                            *scroll_version.borrow_mut() += 1;
                            *last_scroll_request.borrow_mut() = now;
                        }
                        if structural
                            || now.duration_since(*last_live_render.borrow())
                                >= LIVE_RENDER_INTERVAL
                        {
                            *last_live_render.borrow_mut() = now;
                            *render_generation.borrow_mut() += 1;
                            set_render_generation.call(*render_generation.borrow());
                        }
                    }
                    // 覆盖层/加载态存活期间（后台恢复未完成，或快照未达
                    // 的加载空态）：每 90ms 推进 shimmer 光带相位并重渲染
                    // （无协议事件也要动起来）。加载态判定与渲染分支一致：
                    // turns 空 + seed 非空 + 尚未 restore。
                    let loading_state = {
                        let t = transcript.borrow();
                        !seed.is_empty()
                            && t.turns().is_empty()
                            && *last_restored_seed.borrow() != seed
                    };
                    if (recovering.borrow().is_some() || loading_state)
                        && now.duration_since(*last_shimmer_step.borrow())
                            >= SHIMMER_STEP_INTERVAL
                    {
                        *last_shimmer_step.borrow_mut() = now;
                        let next_shimmer_tick = shimmer_tick.borrow().wrapping_add(1);
                        *shimmer_tick.borrow_mut() = next_shimmer_tick;
                        *render_generation.borrow_mut() += 1;
                        set_render_generation.call(*render_generation.borrow());
                    }
                    }));
                    if let Err(payload) = panic_result {
                        let message = payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_string())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown panic".into());
                        log_diag(&format!(
                            "chat_view: on_frame panicked (recovered): {message}"
                        ));
                    }
                }
            }) {
                Ok(r) => {
                    *frame_revoker.borrow_mut() = Some(r);
                    log_diag("chat_view: vsync pump started (CompositionTarget.Rendering)");
                }
                Err(e) => log_diag(&format!("chat_view: on_frame failed: {e}")),
            }
            // BUG-F1 Fix B：卸载不丢投影——组件子树在离开 chat 视图时整体
            // 卸载，use_ref（transcript）随之销毁；此处是唯一能把当前投影
            // 移入 bridge 级缓存的机会（返回 chat 时零等待恢复，随后台快照
            // 刷新校正漂移）。空投影不落缓存（保持「加载会话」语义）。
            Some(Box::new(move || {
                let seed = bridge.core().active_seed();
                let mut cached = std::mem::take(&mut *transcript.borrow_mut());
                if seed.is_empty() || cached.turns().is_empty() {
                    return;
                }
                cached.trim_to_window();
                cache_store(seed, cached);
            }))
        }
    });

    // 投影渲染（reactor diff：只更新变化节点）。
    // 内容态：ListView——WinUI 原生虚拟化，行内容只在滚入视口时构建
    // （长会话不再全量渲染，view 闭包只对 realized 行调用）+ 声明式滚动
    // 请求（跟随尾部：restore 滚底 / 新 turn / live 增长；near_bottom
    // 120px 判定在 reconciler/backend，用户上滚浏览历史时不打扰）。
    let active_seed = bridge.core().active_seed();
    let s = transcript.borrow();
    if s.turns().is_empty() {
        // 空态：无 active seed = 新对话；有 seed 但快照未 restore = 加载中；
        // 快照已 restore 但 turns 为空 = 全新空会话（或已清空），非加载中。
        let label = if active_seed.is_empty() {
            "开始新的对话…"
        } else if *last_restored_seed.borrow() == active_seed {
            "开始新的对话…"
        } else {
            "加载会话…"
        };
        let (title, detail, busy) = if label == "加载会话…" {
            ("正在恢复对话", "正在读取时间线与最近的消息。", true)
        } else {
            (
                "开始新的对话",
                "输入消息，或使用斜杠命令开始一项任务。",
                false,
            )
        };
        // 加载中统一用 shimmer 覆盖层（与后台恢复同款：转圈 + 光带
        // 流动，加载全程有动画，不白屏）。shimmer 相位由 vsync 泵在
        // 加载态期间每帧推进（见回调末尾的 shimmer 驱动）。
        if busy {
            let tick = *shimmer_tick.borrow();
            return qaqh_fluent::loading_overlay("chat-loading", title, "加载中，请稍候", tick)
                .automation_name(title)
                .automation_id("chat-loading")
                .with_key(format!("chat-loading-{active_seed}"));
        }
        return qaqh_fluent::empty_state(title, detail, busy)
            .transition(motion::session_enter(), motion::session_exit())
            .automation_name(title)
            .automation_id("chat-empty")
            .with_key(format!("chat-empty-{active_seed}"));
    }
    let turns: Rc<Vec<Rc<BlockTurnView>>> = {
        let cache_key = (s.mutation_rev(), color_scheme);
        let mut cache = window_cache.borrow_mut();
        match cache.as_ref() {
            Some((key, rc)) if *key == cache_key => rc.clone(),
            _ => {
                let previous_by_id: HashMap<&str, &Rc<BlockTurnView>> = cache
                    .as_ref()
                    .map(|(_, rows)| {
                        rows.iter()
                            .map(|turn| (turn.turn_id.as_str(), turn))
                            .collect()
                    })
                    .unwrap_or_default();
                let rows: Vec<Rc<BlockTurnView>> = s
                    .window_turns()
                    .iter()
                    .map(|turn| {
                        previous_by_id
                            .get(turn.turn_id.as_str())
                            .filter(|old| old.mutation_rev == turn.mutation_rev)
                            .cloned()
                            .cloned()
                            .unwrap_or_else(|| Rc::new(turn.clone()))
                    })
                    .collect();
                let rc = Rc::new(rows);
                *cache = Some((cache_key, rc.clone()));
                rc
            }
        }
    };
    // 顶部预加载后本帧需要锚定补偿：取走挂起标记，把
    // 「原窗口首行」（新下标 = 扩展前移量）锚回原位，视口不跳。
    let anchor_rows = pending_anchor.borrow_mut().take();
    let mut builder = list_view_rc(turns, move |turn: &Rc<BlockTurnView>, _i: usize| {
        // turn 级 memo：mutation_rev 未变 → reconciler 完全跳过 turn_view
        // （不深克隆 RichTextParagraph / 不重跑代码高亮 / 不 diff 行内部），
        // 只有本 turn 内容变化才重建。每次 XAML 提交从「重建整个可见
        // 窗口」降为「重建变化的 1 行」——这是输入卡顿/CPU 满载的根治。
        memo(
            turn_memo,
            TurnProps {
                turn: turn.clone(),
                color_scheme: color_scheme.clone(),
            },
        )
        .with_key(turn.turn_id.clone())
    })
    .with_key_selector(|turn: &Rc<BlockTurnView>| turn.turn_id.clone())
    .selection_mode(SelectionMode::None);
    if let Some(anchor_rows) = anchor_rows {
        builder = builder.preserve_anchor(*scroll_version.borrow(), anchor_rows, 0.0);
    } else if force_tail_version
        .borrow()
        .is_some_and(|v| v <= *scroll_version.borrow())
        && *follow_state.borrow() == FollowState::Following
    {
        // 版本握手用 `<=`（原 `==`）：restore 设 force_tail_version 后，
        // 流式内容增长（extent_changed）/ prepend / output resolve 都会
        // 递增 scroll_version，相等匹配被系统操作打断 → resume 永不贴底
        // （用户反馈"恢复不追踪最新"）。`<=` + Following 态保护：用户上滚
        // （Idle）不触发，恢复跟随或点击"回到最新"后仍消费一次。
        force_tail_version.borrow_mut().take();
        builder = builder.force_tail(*scroll_version.borrow());
    } else if *follow_state.borrow() == FollowState::Following {
        // 显式跟随：仅 Following 态下发（Idle = 用户上滚，不打扰）
        builder = builder.follow_tail(*scroll_version.borrow());
    }
    let transcript_list: Element = builder
        .on_view_changed({
            let follow_state = follow_state.clone();
            let render_generation = render_generation.clone();
            let set_render_generation = set_render_generation.clone();
            move |viewport: TemplatedViewport| {
                // 显式状态机驱动：backend 的 near-tail 判定 → 状态切换 →
                // 重渲染（浮层按钮显隐）。状态切换不产生滚动请求，无反馈环。
                let next = if viewport.following_tail {
                    FollowState::Following
                } else {
                    FollowState::Idle
                };
                if *follow_state.borrow() != next {
                    *follow_state.borrow_mut() = next;
                    *render_generation.borrow_mut() += 1;
                    set_render_generation.call(*render_generation.borrow());
                }
            }
        })
        .on_top_reached({
            let bridge = bridge.clone();
            let transcript = transcript.clone();
            let pending_anchor = pending_anchor.clone();
            let scroll_version = scroll_version.clone();
            let render_generation = render_generation.clone();
            let set_render_generation = set_render_generation.clone();
            move |_| {
                // 滚动接近窗口顶部（边沿触发一次）：先扩展窗口内预加载更早
                // 回合，渲染时锚定补偿保持视口。
                let mut t = transcript.borrow_mut();
                let moved = t.expand_window(WINDOW_PAGE);
                if moved > 0 {
                    *pending_anchor.borrow_mut() = Some(moved);
                    // 锚定补偿随 scroll_version 变化触发（reconciler 按版本
                    // diff）；独立 UI 代次保证触发渲染。
                    *scroll_version.borrow_mut() += 1;
                    drop(t);
                    *render_generation.borrow_mut() += 1;
                    set_render_generation.call(*render_generation.borrow());
                    return;
                }
                drop(t);
                // 窗口内已全量放行：若服务端还有更早回合 → 翻页拉取
                // （异步前插，bridge 在途防重入 + has_more 自动维护）。
                let seed = bridge.core().active_seed();
                if seed.is_empty() || !bridge.core().timeline_has_more(&seed) {
                    return;
                }
                let before = transcript
                    .borrow()
                    .turns()
                    .first()
                    .map(|t| t.turn_id.clone());
                if let Some(before) = before {
                    bridge.core().spawn_fetch_earlier(&seed, &before);
                }
            }
        })
        .top_threshold(NEAR_TOP_THRESHOLD_PX)
        .with_key(format!("chat-transcript-{active_seed}"))
        .transition(motion::session_enter(), motion::session_exit())
        .into();
    // 显式跟随入口：Idle 时浮层"回到最新"按钮（点击 → force_tail +
    // 重新跟随；跟随态隐藏，不挡内容）
    let jump_button: Element = if *follow_state.borrow() == FollowState::Idle {
        let btn: Element = button("回到最新 ↓")
            .on_click({
                let follow_state = follow_state.clone();
                let scroll_version = scroll_version.clone();
                let force_tail_version = force_tail_version.clone();
                let render_generation = render_generation.clone();
                let set_render_generation = set_render_generation.clone();
                move || {
                    *scroll_version.borrow_mut() += 1;
                    *force_tail_version.borrow_mut() = Some(*scroll_version.borrow());
                    *follow_state.borrow_mut() = FollowState::Following;
                    *render_generation.borrow_mut() += 1;
                    set_render_generation.call(*render_generation.borrow());
                }
            })
            .into();
        btn.vertical_alignment(VerticalAlignment::Bottom)
            .horizontal_alignment(HorizontalAlignment::Center)
            .margin(Thickness {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: tokens::SPACE_4,
            })
            .with_key("jump-tail")
    } else {
        Element::Empty
    };
    // 后台恢复覆盖层：快照未到达期间盖住内容（半透明 + 转圈 + shimmer
    // 光带），不白屏不闪烁；restore 完成后同帧熄灭（见恢复分支）。
    let recovery_overlay: Element = if recovering.borrow().as_ref() == Some(&active_seed) {
        let tick = *shimmer_tick.borrow();
        qaqh_fluent::loading_overlay("recovery-overlay", "正在恢复对话…", "恢复中，请稍候", tick)
    } else {
        Element::Empty
    };
    let transcript: Element = grid([transcript_list, jump_button, recovery_overlay])
        .columns([GridLength::Star(1.0)])
        .rows([GridLength::Star(1.0)])
        .into();
    transcript
        .automation_name("对话记录")
        .automation_id("chat-transcript")
}
