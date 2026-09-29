# Public Surface Design System

本文档定义 Monoize **公开站**的视觉语言。控制台（`/dashboard`）不受本文档影响。
规范优先级：`spec/frontend-design-system.spec.md` 与 `spec/public-site.spec.md`。冲突时以 spec 为准。
来源与实测参考见 `design-experimental.md`。

## 1. 设计方向

极简单色（参考 OpenAI / Kimi 官网）：近黑或近白底、单色文字、全无衬线、胶囊按钮、细线分区、
大留白。页面不出现彩色主色、渐变、网格纹理和光晕。唯一「视觉」是代码块与数据列表。

## 2. 作用范围

- 适用：`/`、`/marketplace`、`/status`、`/apidocs`、`/usage-ranking`、`/login`。
- 不适用：`/dashboard`、`/settings`、`/sales`。
- 实现：`PublicLayout` 与登录页根元素加 `public-surface`，token 由该类覆盖。

## 3. 颜色

| Token | 浅色 | 深色 | 用途 |
| --- | --- | --- | --- |
| `background` | `0 0% 98%` | `0 0% 4%` | 页面底色 |
| `foreground` | `0 0% 9%` | `0 0% 93%` | 正文 |
| `card` | `0 0% 100%` | `0 0% 6%` | 面板/代码块 |
| `primary` | `0 0% 9%` | `0 0% 93%` | 主行动、焦点环 |
| `muted` | `0 0% 96%` | `0 0% 10%` | 次级底色 |
| `muted-foreground` | `0 0% 45%` | `0 0% 64%` | 次要文本 |
| `border` | `0 0% 90%` | `0 0% 15%` | 边框、分隔线 |
| `radius` | `0.75rem` | `0.75rem` | 圆角基准 |

规则：只有黑/白/灰。状态色沿用全局语义 token，且只用于有状态语义处。

## 4. 排版

| 角色 | 字体 | 字号 | 字重 | 行高 | 字距 |
| --- | --- | --- | --- | --- | --- |
| Hero 标题 | 无衬线 | `clamp(2.25rem, 5vw, 3rem)` | 500 | 1.08 | `-0.025em` |
| 区块标题 | 无衬线 | `1.875rem`–`2.25rem` | 500 | 1.15 | `-0.02em` |
| 正文 | 无衬线 | `16px` | 400 | 1.6 | `0` |
| 标签/序号 | 等宽 | `12px` | 500 | 1 | `0.16em` |
| 代码/价格 | 等宽 | `13`–`14px` | 400 | 1.7 | `0` |

- 不使用衬线字体。
- 单栏正文最大宽度 `60ch`；标题左对齐（Hero 居中）。

## 5. 布局与结构

- 容器最大宽度 `max-w-5xl`；内边距 `20/32/40px`。
- 区块用 `1px border` 或 `80–96px` 留白分隔。
- 模型用三张卡片（Provider 标签 + 模型名 + 说明 + 价格）；能力用「序号 + 标题 + 说明」横排。
- 数据行用四栏统计，数字进入视口时 count-up。
- FAQ 用原生 `details`，展开用 `max-height + opacity` 过渡。
- 页脚为四列导航 + 底部品牌行。

## 6. 组件

- 主按钮：胶囊（`rounded-full`），实底 `primary`。
- 次按钮：胶囊，透明底 + `1px border`。
- 面板：`card` 底 + `1px border` + `rounded-xl`，无阴影。
- 标签 chip：胶囊，`1px border`，`muted-foreground`。
- 表格行：hover 用 `bg-muted/30`。

## 7. 动效

- Hero：eyebrow / 标题 / 说明 / 按钮 / 提示依次淡入上移（0.5–0.7s，easeOutExpo，错峰 0.12s）。
- 区块：进入视口一次性淡入上移（复用 `ScrollReveal`）。
- 模型跑马灯：两条相同内容横向位移 50%，30s 线性循环，hover 暂停，两端渐隐遮罩。
- 统计数字：进入视口时 count-up（900ms，easeOutCubic）。
- 卡片/行 hover：背景或边框变化；按钮 hover 轻微上移。
- `prefers-reduced-motion: reduce` 时关闭位移与跑马灯，数字直接显示终值。
- 整页背景：可选「字符海洋」canvas（PS-V7），固定视口、置于全部内容之后。默认缓慢流动，鼠标轨迹产生涟漪与高亮；单色低对比，离屏/隐藏暂停，reduced-motion 只渲染静态一帧。

## 8. 反模式

- 不用彩色主色、渐变、网格纹理、光晕。
- 不用衬线字体。
- 不用阴影（`shadow` 及以上）。
- 不用同质圆角卡片网格堆叠。
- 不用大号背景数字/水印。

## 9. 实现位置

- `frontend/src/index.css`：`.public-surface` token 覆盖；`.home-marquee` 与 `.home-faq` 样式。
- `frontend/src/pages/public-layout.tsx`：根元素 `public-surface`；导航胶囊按钮。
- `frontend/src/pages/welcome.tsx`：首页全部区块与动效。
- `frontend/src/locales/*.json`：`publicSite.home.*` 文案（四语言）。
