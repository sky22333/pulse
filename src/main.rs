extern crate gpui_kit as gpui;
mod analysis;
mod capture;
mod recording_file;
use analysis::{Analysis, PlotPoint, analyze};
use capture::{Capture, Kind, Recording};
use gpui::{
    App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Disableable, Selectable, TitleBar,
    button::{Button, ButtonCustomVariant, ButtonVariants},
};

gpui_kit::assets::icon_assets!(ExtraIcons, [Mouse, Keyboard, Download]);

// GPUI Kit's default bundle contains its own icons, not the full Lucide
// catalog. Supply our three additional icons without embedding the whole set.
struct PulseAssets;
impl gpui::AssetSource for PulseAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(icon) = gpui::AssetSource::load(&ExtraIcons, path)? {
            return Ok(Some(icon));
        }
        gpui::AssetSource::load(&gpui_kit::assets::Assets, path)
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        let mut paths = gpui::AssetSource::list(&gpui_kit::assets::Assets, path)?;
        paths.extend(gpui::AssetSource::list(&ExtraIcons, path)?);
        Ok(paths)
    }
}

fn quality_message(recording: &Recording, analysis: &Analysis) -> &'static str {
    if recording.dropped > 0 || recording.errors > 0 {
        "采集存在错误或内部缓冲区丢弃，结果可能不完整。"
    } else if analysis.count < 2 || analysis.seconds <= 0. {
        "有效事件不足，请重新测试"
    } else if recording.kind == Kind::Mouse && analysis.coincident_gaps > 0 {
        "部分事件时间重叠，间隔可能不准确"
    } else if recording.kind == Kind::Mouse && (analysis.count < 100 || analysis.seconds < 1.) {
        "采样较少，建议持续移动至少 1 秒后重测"
    } else if recording.kind == Kind::Mouse
        && analysis.median_ms > 0.
        && analysis.p95_ms > analysis.median_ms * 4.
    {
        "事件间隔波动较大，请持续移动后重测"
    } else {
        "采集完成"
    }
}

struct Pulse {
    kind: Kind,
    capture: Option<Capture>,
    recording: Option<Recording>,
    result: Option<Analysis>,
    busy: bool,
    hover: Option<(usize, usize)>,
    message: String,
}
impl Pulse {
    fn new() -> Self {
        Self {
            kind: Kind::Mouse,
            capture: None,
            recording: None,
            result: None,
            busy: false,
            hover: None,
            message: "选择类型，点击右上角开始".into(),
        }
    }
    fn switch_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.hover = None;
        self.message = match (self.recording.as_ref(), self.result.as_ref()) {
            (Some(recording), Some(result)) if recording.kind == kind => {
                quality_message(recording, result).into()
            }
            _ => "选择类型，点击右上角开始".into(),
        };
        cx.notify();
    }
    fn toggle(&mut self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(c) = self.capture.take() {
            self.busy = true;
            self.message = "正在分析采集数据……".into();
            let task = cx.background_executor().spawn(async move {
                c.stop().map(|r| {
                    let a = analyze(&r);
                    (r, a)
                })
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |s, cx| {
                    s.busy = false;
                    match result {
                        Ok((r, a)) => {
                            s.message = quality_message(&r, &a).into();
                            s.result = Some(a);
                            s.recording = Some(r);
                        }
                        Err(e) => s.message = e,
                    };
                    cx.notify();
                });
            })
            .detach();
        } else {
            match Capture::start(self.kind) {
                Ok(c) => {
                    let flag = c.first_event();
                    self.capture = Some(c);
                    self.recording = None;
                    self.result = None;
                    self.hover = None;
                    self.message = if self.kind == Kind::Mouse {
                        "请持续移动要测试的鼠标，完成后点击停止"
                    } else {
                        "请按下要测试的键盘按键，完成后点击停止"
                    }
                    .into();
                    cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(250))
                                .await;
                            let done = flag.load(std::sync::atomic::Ordering::Relaxed);
                            let current = this
                                .update(cx, |s, cx| {
                                    let current = s.capture.as_ref().is_some_and(|c| {
                                        std::sync::Arc::ptr_eq(&flag, &c.first_event())
                                    });
                                    if current && done {
                                        s.message = "已收到输入，继续操作；完成后点击停止".into();
                                        cx.notify();
                                    }
                                    current
                                })
                                .unwrap_or(false);
                            if done || !current {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                Err(e) => self.message = e,
            }
        }
        cx.notify();
    }
    fn open(&mut self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.capture.is_some() {
            return;
        }
        let choice = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("打开 Pulse 录制文件".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = choice.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |s, cx| s.import_path(path, cx));
            }
        })
        .detach();
    }
    fn import_path(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        if self.busy || self.capture.is_some() {
            return;
        }
        self.busy = true;
        self.message = "正在读取并分析录制数据……".into();
        let task = cx.background_executor().spawn(async move {
            recording_file::load(&path).map(|r| {
                let a = analyze(&r);
                (r, a)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy = false;
                match result {
                    Ok((r, a)) => {
                        s.kind = r.kind;
                        s.hover = None;
                        s.message = quality_message(&r, &a).into();
                        s.recording = Some(r);
                        s.result = Some(a);
                    }
                    Err(e) => s.message = format!("导入失败：{e}"),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn export(&mut self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.capture.is_some() || self.recording.is_none() {
            return;
        }
        let initial = std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let choice = cx.prompt_for_new_path(&initial, Some("Pulse.json"));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = choice.await {
                let _ = this.update(cx, |s, cx| s.export_path(path, cx));
            }
        })
        .detach();
    }
    fn export_path(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        if self.busy || self.capture.is_some() {
            return;
        }
        let (Some(recording), Some(analysis)) = (self.recording.take(), self.result.take()) else {
            return;
        };
        self.busy = true;
        self.message = "正在保存…".into();
        let task = cx.background_executor().spawn(async move {
            let result = recording_file::save(&path, &recording, &analysis);
            (recording, analysis, result)
        });
        cx.spawn(async move |this, cx| {
            let (recording, analysis, result) = task.await;
            let _ = this.update(cx, |s, cx| {
                let quality = quality_message(&recording, &analysis);
                s.recording = Some(recording);
                s.result = Some(analysis);
                s.busy = false;
                s.message = match result {
                    Ok(()) => format!("已保存 · {quality}"),
                    Err(e) => format!("保存失败：{e}"),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
fn muted(s: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .text_color(rgb(0x888c8f))
        .text_size(px(12.))
        .child(s.into())
}
fn icon_button(id: &'static str, icon: IconName, name: &'static str, cx: &App) -> Button {
    let palette = ButtonCustomVariant::new(cx)
        .color(gpui::transparent_black())
        .foreground(rgb(0x414b61).into())
        .hover(gpui::rgba(0x8f9fe01c).into())
        .active(gpui::rgba(0x8f9fe036).into());
    Button::new(id)
        .custom(palette)
        .icon(icon)
        .tooltip(name)
        .accessibility_label(name)
}
fn plot_domain(index: usize) -> (f32, f32) {
    match index {
        0 => (0., 20.),
        1 => (0., 100.),
        2 => (0., 1000.),
        _ => (50., 8000.),
    }
}
fn point_ratio(x: f32, domain: (f32, f32)) -> f32 {
    ((x - domain.0) / (domain.1 - domain.0)).clamp(0., 1.)
}
fn hover_index(
    x: f32,
    y: f32,
    bounds: Bounds<gpui::Pixels>,
    points: &[PlotPoint],
    domain: (f32, f32),
    max: f32,
) -> Option<usize> {
    let left = bounds.origin.x.as_f32();
    let top = bounds.origin.y.as_f32();
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    if width <= 0. || height <= 0. || x < left || x > left + width || y < top || y > top + height {
        return None;
    }
    points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let dx = left + width * point_ratio(p.x, domain) - x;
            let dy = top + height - 2. - (p.y / max) * (height - 4.) - y;
            let distance = dx * dx + dy * dy;
            (distance <= 18. * 18.).then_some((i, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

fn graph(
    plot: (usize, Option<(usize, usize)>, f32, bool),
    title: &'static str,
    range: &'static str,
    points: Option<&Vec<PlotPoint>>,
    peak: Option<f64>,
    color: u32,
    cx: &mut Context<Pulse>,
) -> gpui::Div {
    let (index, hover, height, sampled) = plot;
    let domain = plot_domain(index);
    use std::{cell::Cell, rc::Rc};
    let has_data = points.is_some();
    let data = points
        .cloned()
        .unwrap_or_else(|| vec![PlotPoint::default(); 160]);
    let max = data.iter().map(|p| p.y).fold(0_f32, f32::max).max(1.);
    let active = hover
        .filter(|(plot, _)| *plot == index)
        .map(|(_, i)| i)
        .filter(|i| *i < data.len());
    let label = if let Some(i) = active.filter(|_| has_data) {
        let p = data[i];
        if index < 3 {
            if index == 1 && sampled {
                format!("区间峰值 {:.2} ms · 约 {:.1} 次", p.x, p.y)
            } else {
                format!("区间峰值 {:.2} ms · {:.0} 次", p.x, p.y)
            }
        } else {
            format!("区间峰值 {:.0} Hz · 幅值 {:.1}", p.x, p.y)
        }
    } else {
        peak.map(|v| {
            if index < 3 {
                format!("峰值 {v:.2} ms")
            } else {
                format!("峰值 {v:.0} Hz")
            }
        })
        .unwrap_or_default()
    };
    let bounds_cell: Rc<Cell<Option<Bounds<gpui::Pixels>>>> = Rc::new(Cell::new(None));
    let observed = bounds_cell.clone();
    let mouse_bounds = bounds_cell.clone();
    let mouse_data = data.clone();
    let paint_data = data;
    let plot = div()
        .id(("plot", index))
        .debug_selector(move || format!("plot-{index}"))
        .w_full()
        .h(px(height))
        .on_mouse_move(cx.listener(move |s, ev: &gpui::MouseMoveEvent, _, cx| {
            if !has_data {
                return;
            }
            if let Some(b) = mouse_bounds.get() {
                let next = hover_index(
                    ev.position.x.as_f32(),
                    ev.position.y.as_f32(),
                    b,
                    &mouse_data,
                    domain,
                    max,
                )
                .map(|v| (index, v));
                if s.hover != next {
                    s.hover = next;
                    cx.notify();
                }
            }
        }))
        .on_hover(cx.listener(move |s, inside: &bool, _, cx| {
            if !*inside && s.hover.is_some_and(|(plot, _)| plot == index) {
                s.hover = None;
                cx.notify();
            }
        }))
        .child(
            gpui::canvas(
                move |bounds, _, _| {
                    observed.set(Some(bounds));
                },
                move |bounds, _, window, _| {
                    let w = bounds.size.width.as_f32();
                    let h = bounds.size.height.as_f32();
                    for y in [0_f32, 0.5, 1.] {
                        window.paint_quad(gpui::fill(
                            Bounds {
                                origin: gpui::point(
                                    bounds.origin.x,
                                    bounds.origin.y + px((h - 1.) * y),
                                ),
                                size: size(px(w), px(1.)),
                            },
                            rgb(0xe2e5ef),
                        ));
                    }
                    if has_data && paint_data.len() > 1 && w > 0. {
                        let mut path = gpui::PathBuilder::stroke(px(1.5));
                        for (i, p) in paint_data.iter().enumerate() {
                            let x = bounds.origin.x + px(w * point_ratio(p.x, domain));
                            let y = bounds.origin.y + px(h - 2. - (p.y / max) * (h - 4.));
                            let pos = gpui::point(x, y);
                            if i == 0 {
                                path.move_to(pos)
                            } else {
                                path.line_to(pos)
                            };
                        }
                        if let Ok(path) = path.build() {
                            window.paint_path(path, rgb(color));
                        }
                        if let Some(i) = active {
                            let x = bounds.origin.x + px(w * point_ratio(paint_data[i].x, domain));
                            let y =
                                bounds.origin.y + px(h - 2. - (paint_data[i].y / max) * (h - 4.));
                            window.paint_quad(gpui::fill(
                                Bounds {
                                    origin: gpui::point(x - px(3.), y - px(3.)),
                                    size: size(px(6.), px(6.)),
                                },
                                rgb(color),
                            ));
                        }
                    }
                },
            )
            .size_full(),
        );
    div()
        .flex_1()
        .min_w(px(0.))
        .px_4()
        .py_2()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(0x434a59))
                        .child(title),
                )
                .child(muted(label)),
        )
        .child(plot)
        .child(
            div()
                .flex()
                .justify_between()
                .child(muted(if index >= 3 { "50 Hz" } else { "0" }))
                .child(muted(range)),
        )
}

impl Render for Pulse {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let graph_height = ((window.viewport_size().height.as_f32() - 460.) / 2.).clamp(40., 120.);
        let mouse = self.kind == Kind::Mouse;
        let running = self.capture.is_some();
        let busy = self.busy;
        let analysis = self
            .recording
            .as_ref()
            .filter(|r| r.kind == self.kind)
            .and(self.result.as_ref());
        let number = analysis
            .map(|a| {
                if mouse {
                    if a.count > 1 && a.seconds > 0. {
                        format!("{:.0}", a.rate)
                    } else {
                        "—".into()
                    }
                } else {
                    format!("{}", a.count)
                }
            })
            .unwrap_or_else(|| "—".into());
        let summary = analysis
            .map(|a| {
                if a.count < 2 {
                    format!("{} 个事件 · 样本不足", a.count)
                } else {
                    format!(
                        "{} 个事件   ·   {:.2} 秒   ·   中位间隔 {:.3} 毫秒",
                        a.count, a.seconds, a.median_ms
                    )
                }
            })
            .unwrap_or_default();
        let charts = analysis.filter(|a| !a.time_charts.is_empty()).map(|a| {
            let t = |i| a.time_charts.get(i);
            let f = |i| a.frequency_charts.get(i);
            let tp = |i| a.time_peaks_ms.get(i).copied().flatten();
            let fp = |i| a.frequency_peaks_hz.get(i).copied().flatten();
            let h = self.hover;
            let dims = |index| (index, h, graph_height, a.pairs_sampled);
            let g0 = graph(
                dims(0),
                "相邻事件时间差",
                "20 毫秒",
                t(0),
                tp(0),
                0x668cc4,
                cx,
            );
            let g1 = graph(
                dims(1),
                "事件对时间差",
                "100 毫秒",
                t(1),
                tp(1),
                0xb592c3,
                cx,
            );
            let g2 = graph(dims(2), "事件相位", "1 秒", t(2), tp(2), 0x8295c8, cx);
            let g3 = graph(
                dims(3),
                "相邻时间差频谱",
                "8 kHz",
                f(0),
                fp(0),
                0x668cc4,
                cx,
            );
            let g4 = graph(
                dims(4),
                "事件对差值频谱",
                "8 kHz",
                f(1),
                fp(1),
                0xb592c3,
                cx,
            );
            let g5 = graph(dims(5), "相位频谱", "8 kHz", f(2), fp(2), 0x8295c8, cx);
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .mx_8()
                        .pt_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("输入分析"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(muted("悬停曲线查看数值"))
                                .child(icon_button(
                                    "info",
                                    IconName::Info,
                                    "横轴为时间或频率，纵轴为次数或频谱幅值；每点为区间峰值。频谱幅值未归一化。",
                                    cx,
                                )),
                        ),
                )
                .child(div().mx_4().mt_2().flex().child(g0).child(g1).child(g2))
                .child(div().mx_4().flex().child(g3).child(g4).child(g5))
        });
        let bg = gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(rgb(0xe4edff), 0.),
            gpui::linear_color_stop(rgb(0xede7ff), 1.),
        );
        let fade = gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(gpui::rgba(0xffffff00), 0.),
            gpui::linear_color_stop(gpui::rgba(0xffffffff), 1.),
        );
        div()
            .size_full()
            .relative()
            .bg(bg)
            .text_color(rgb(0x262d3a))
            .font_family("Microsoft YaHei UI")
            .flex()
            .flex_col()
            .child(div().absolute().inset_0().bg(fade))
            .child(TitleBar::new().bg(gpui::transparent_black()))
            .child(
                div()
                    .id("content")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(56.))
                            .flex_shrink_0()
                            .px_8()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_size(px(17.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Pulse"),
                                    )
                                    .child(muted(if mouse {
                                        "鼠标测试"
                                    } else {
                                        "键盘测试"
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        icon_button("mouse", IconName::Mouse, "切换到鼠标测试", cx)
                                            .selected(mouse)
                                            .disabled(running || busy)
                                            .on_click(cx.listener(|s, _, _, cx| {
                                                s.switch_kind(Kind::Mouse, cx);
                                            })),
                                    )
                                    .child(
                                        icon_button(
                                            "keyboard",
                                            IconName::Keyboard,
                                            "切换到键盘测试",
                                            cx,
                                        )
                                        .selected(!mouse)
                                        .disabled(running || busy)
                                        .on_click(
                                            cx.listener(|s, _, _, cx| {
                                                s.switch_kind(Kind::Keyboard, cx);
                                            }),
                                        ),
                                    )
                                    .child(div().h(px(20.)).w(px(1.)).mx_2().bg(rgb(0xcfd6e5)))
                                    .child(
                                        icon_button(
                                            "open",
                                            IconName::FolderOpen,
                                            "打开录制文件",
                                            cx,
                                        )
                                        .disabled(running || busy)
                                        .on_click(cx.listener(Self::open)),
                                    )
                                    .child(
                                        icon_button(
                                            "export",
                                            IconName::Download,
                                            "保存录制文件",
                                            cx,
                                        )
                                        .disabled(running || busy || analysis.is_none())
                                        .on_click(cx.listener(Self::export)),
                                    )
                                    .child(
                                        icon_button(
                                            "record",
                                            if running {
                                                IconName::Square
                                            } else {
                                                IconName::Play
                                            },
                                            if running {
                                                "停止测试"
                                            } else if analysis.is_some() {
                                                "重新测试"
                                            } else {
                                                "开始测试"
                                            },
                                            cx,
                                        )
                                        .selected(running)
                                        .disabled(busy)
                                        .on_click(cx.listener(Self::toggle)),
                                    ),
                            ),
                    )
                    .when(analysis.is_some(), |d| {
                        d.child(
                            div()
                                .id("metric-summary")
                                .debug_selector(|| "metric-summary".to_string())
                                .px_8()
                                .pt_4()
                                .pb_3()
                                .flex()
                                .items_end()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(muted(if mouse {
                                            "输入事件速率"
                                        } else {
                                            "已记录的键盘事件"
                                        }))
                                        .child(
                                            div()
                                                .flex()
                                                .items_end()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_size(px(36.))
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .child(number),
                                                )
                                                .child(div().pb_2().child(muted(if mouse {
                                                    "事件 / 秒"
                                                } else {
                                                    "次"
                                                }))),
                                        )
                                        .child(muted(summary)),
                                )
                                .child(muted(if running {
                                    "正在采集"
                                } else if busy {
                                    "正在处理"
                                } else {
                                    "就绪"
                                })),
                        )
                    })
                    .when(busy, |d| {
                        d.child(
                            div()
                                .mx_8()
                                .h(px(8.))
                                .w(px(180.))
                                .rounded_md()
                                .bg(rgb(0xe0e5f0)),
                        )
                    })
                    .when_some(charts, |d, charts| d.child(charts))
                    .when(analysis.is_none() && !busy, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_h(px(180.))
                                .pb(px(56.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .flex_col()
                                .gap_3()
                                .child(
                                    div()
                                        .id("empty-heading")
                                        .debug_selector(|| "empty-heading".to_string())
                                        .text_size(px(26.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(0x313e57))
                                        .child(if running {
                                            "正在采集"
                                        } else {
                                            "准备好测试了吗？"
                                        }),
                                )
                                .child(muted(self.message.clone())),
                        )
                    })
                    .when(analysis.is_some() || busy, |d| {
                        d.child(
                            div()
                                .id("status-footer")
                                .debug_selector(|| "status-footer".to_string())
                                .mx_8()
                                .mt_2()
                                .pt_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_1()
                                .child(muted(self.message.clone()))
                                .when(analysis.is_some_and(|a| a.pairs_sampled), |d| {
                                    d.child(muted("事件对图表使用抽样，结果为近似值。"))
                                }),
                        )
                    }),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(PulseAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            let bounds = Bounds::centered(None, size(px(900.), px(650.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(760.), px(560.))),
                    ..TitleBar::window_options()
                },
                cx,
                |_, cx| cx.new(|_| Pulse::new()),
            )
            .unwrap();
            cx.activate(true);
        });
}

#[cfg(test)]
mod chart_tests {
    use super::hover_index;
    #[test]
    fn hover_selects_actual_frequency_position() {
        use super::PlotPoint;
        let points = [
            PlotPoint { x: 50., y: 1. },
            PlotPoint { x: 120., y: 4. },
            PlotPoint { x: 8000., y: 2. },
        ];
        let bounds = gpui::Bounds {
            origin: gpui::point(gpui::px(50.), gpui::px(20.)),
            size: gpui::size(gpui::px(100.), gpui::px(100.)),
        };
        let x = 50. + (120. - 50.) / 7950. * 100.;
        assert_eq!(
            hover_index(x, 22., bounds, &points, (50., 8000.), 4.),
            Some(1)
        );
        assert_eq!(hover_index(x, 75., bounds, &points, (50., 8000.), 4.), None);
        assert_eq!(
            hover_index(49., 22., bounds, &points, (50., 8000.), 4.),
            None
        );
    }
    #[gpui::test]
    fn hovering_plot_updates_selected_point(cx: &mut gpui::TestAppContext) {
        use super::*;
        use crate::capture::Sample;
        cx.update(gpui_kit::init);
        let (entity, view) = cx.add_window_view(|_, _| {
            let mut app = Pulse::new();
            let rec = Recording {
                kind: Kind::Mouse,
                frequency: 1_000_000,
                device: 1,
                samples: vec![
                    Sample {
                        ticks: 1000,
                        code: 0,
                        flags: 0,
                        x: 1,
                        y: 0,
                    },
                    Sample {
                        ticks: 2000,
                        code: 0,
                        flags: 0,
                        x: 1,
                        y: 0,
                    },
                ],
                dropped: 0,
                errors: 0,
            };
            app.result = Some(analyze(&rec));
            app.recording = Some(rec);
            app
        });
        view.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = view.debug_bounds("plot-0").expect("plot bounds");
        view.simulate_mouse_move(
            gpui::point(
                bounds.origin.x + bounds.size.width * 0.05,
                bounds.origin.y + gpui::px(2.),
            ),
            None,
            Default::default(),
        );
        assert!(entity.read_with(view, |app, _| app.hover.is_some_and(|(plot, _)| plot == 0)));
    }
    #[test]
    fn all_toolbar_icons_are_bundled() {
        use super::*;
        use gpui::AssetSource;
        for icon in [
            IconName::Mouse,
            IconName::Keyboard,
            IconName::Download,
            IconName::FolderOpen,
            IconName::Play,
            IconName::Square,
            IconName::Info,
        ] {
            assert!(
                PulseAssets.load(&icon.path()).unwrap().is_some(),
                "{:?}",
                icon
            );
        }
    }
    #[gpui::test]
    fn empty_state_does_not_create_graphs(cx: &mut gpui::TestAppContext) {
        use super::*;
        cx.update(gpui_kit::init);
        let (_, view) = cx.add_window_view(|_, _| Pulse::new());
        view.update(|window, cx| window.draw(cx).clear(cx));
        assert!(view.debug_bounds("plot-0").is_none());
    }
    #[gpui::test]
    fn results_fit_default_and_minimum_window(cx: &mut gpui::TestAppContext) {
        use super::*;
        use crate::capture::Sample;
        cx.update(gpui_kit::init);
        let (_, view) = cx.add_window_view(|_, _| {
            let mut app = Pulse::new();
            let rec = Recording {
                kind: Kind::Mouse,
                frequency: 1_000_000,
                device: 1,
                samples: vec![
                    Sample {
                        ticks: 1000,
                        code: 0,
                        flags: 0,
                        x: 1,
                        y: 0,
                    },
                    Sample {
                        ticks: 2000,
                        code: 0,
                        flags: 0,
                        x: 1,
                        y: 0,
                    },
                ],
                dropped: 0,
                errors: 0,
            };
            let mut result = analyze(&rec);
            result.pairs_sampled = true;
            app.result = Some(result);
            app.recording = Some(rec);
            app
        });
        for (width, height) in [(900., 650.), (760., 560.)] {
            view.simulate_resize(size(px(width), px(height)));
            view.update(|window, cx| window.draw(cx).clear(cx));
            let footer = view.debug_bounds("status-footer").expect("status footer");
            for name in ["plot-0", "plot-1", "plot-2", "plot-3", "plot-4", "plot-5"] {
                let plot = view.debug_bounds(name).expect("graph bounds");
                assert!(
                    plot.bottom() <= px(height) && plot.right() <= px(width),
                    "{name} outside {width}x{height}: {plot:?}"
                );
            }
            assert!(
                footer.bottom() <= px(height),
                "footer outside {width}x{height}: {footer:?}"
            );
        }
    }
    #[gpui::test]
    fn initial_prompt_is_centered_without_placeholder_metric(cx: &mut gpui::TestAppContext) {
        use super::*;
        cx.update(gpui_kit::init);
        let (_, view) = cx.add_window_view(|_, _| Pulse::new());
        view.simulate_resize(size(px(900.), px(650.)));
        view.update(|window, cx| window.draw(cx).clear(cx));
        assert!(view.debug_bounds("metric-summary").is_none());
        let heading = view.debug_bounds("empty-heading").expect("empty heading");
        assert!((heading.center().x.as_f32() - 450.).abs() < 2.);
        assert!((heading.center().y.as_f32() - 325.).abs() < 45.);
    }
    #[test]
    fn timing_warning_is_not_confused_with_usb_loss() {
        use super::*;
        let r = Recording {
            kind: Kind::Mouse,
            frequency: 1_000_000,
            device: 1,
            samples: vec![],
            dropped: 0,
            errors: 0,
        };
        let a = Analysis {
            coincident_gaps: 1,
            count: 2,
            seconds: 1.,
            ..Default::default()
        };
        assert_eq!(quality_message(&r, &a), "部分事件时间重叠，间隔可能不准确");
    }
    #[test]
    fn quality_distinguishes_short_unstable_and_normal_results() {
        use super::*;
        let r = Recording {
            kind: Kind::Mouse,
            frequency: 1_000_000,
            device: 1,
            samples: vec![],
            dropped: 0,
            errors: 0,
        };
        let mut a = Analysis::default();
        assert_eq!(quality_message(&r, &a), "有效事件不足，请重新测试");
        a.count = 20;
        a.seconds = 0.5;
        assert!(quality_message(&r, &a).contains("采样较少"));
        a.count = 1000;
        a.seconds = 2.;
        a.median_ms = 1.;
        a.p95_ms = 5.;
        assert!(quality_message(&r, &a).contains("波动较大"));
        a.p95_ms = 1.2;
        assert_eq!(quality_message(&r, &a), "采集完成");
    }
}
