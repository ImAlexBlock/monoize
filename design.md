# Public Surface Design System

本文档定义 Monoize **公开站**的视觉语言。控制台（`/dashboard`）不受本文档影响。
规范优先级：`spec/frontend-design-system.spec.md`。公开站的实现范围与无障碍要求见
`spec/public-site.spec.md`。冲突时以 spec 为准。

## 1. 设计方向

编辑部 / 印刷感：纸面、墨字、细线、大留白。取消渐变光晕、网格纹理、玻璃拟态和大圆角。
强调色使用赤陶橙（terracotta），不使用默认的科技蓝。

可观察特征（不是形容词）：

- 标题使用衬线字体，显著大于正文。
- 区块之间由 1px 细线或留白分隔，而不是同质卡片网格。
- 圆角接近直角（2px）。
- 页面不出现径向渐变、彩色阴影、模糊光斑。

## 2. 作用范围

- 适用：`/`、`/marketplace`、`/status`、`/apidocs`、`/usage-ranking`。
- 不适用：`/dashboard`、`/settings`、`/sales`。
- 实现方式：在 `PublicLayout` 根元素加 `public-surface` 类，token 通过该类覆盖。

## 3. 颜色

通过 `.public-surface` 覆盖语义 token。浅色为纸面、深色为墨色。

| Token | 浅色 | 深色 | 用途 |
| --- | --- | --- | --- |
| `background` | `40 24% 97%` | `30 8% 8%` | 页面底色 |
| `foreground` | `30 10% 12%` | `40 20% 92%` | 正文 |
| `card` | `40 30% 99%` | `30 8% 11%` | 卡片/代码块底色 |
| `primary` | `16 70% 44%` | `16 72% 58%` | 主行动、链接、焦点环 |
| `muted` | `40 18% 94%` | `30 6% 15%` | 次级底色 |
| `muted-foreground` | `30 8% 40%` | `40 8% 62%` | 次要文本 |
| `border` | `32 14% 85%` | `30 6% 20%` | 边框、分隔线 |
| `radius` | `0.125rem` | `0.125rem` | 圆角基准 |

规则：

- 只有赤陶橙一个强调色，不新增其他色相；状态色沿用全局语义状态 token。
- 不使用 Tailwind `blue-*` 或原始调色板。

## 4. 排版

| 角色 | 字体 | 字号 | 字重 | 行高 |
| --- | --- | --- | --- | --- |
| Hero 标题 | `font-display` | `clamp(3rem, 6vw, 4.5rem)` | 600 | 1.05 |
| 区块标题 | `font-display` | `2rem`–`2.5rem` | 600 | 1.15 |
| 正文 | `font-sans-cjk` | `1rem` | 400 | 1.75 |
| 标签/序号 | `font-code` | `0.75rem` | 500 | 1 |
| 代码 | `font-code` | `0.875rem` | 400 | 1.75 |

规则：

- 标题左对齐；Hero 之外不使用居中标题。
- 标签一律大写并加字距 `0.18em`。
- 正文单栏最大宽度 `68ch`。
- 大号数字背景板保持低透明度，置于标题块后方（`spec/public-site.spec.md` PS-W4）。

## 5. 布局与结构

- 区块分隔优先使用 1px `border` 横线；同一视觉组内用 `48px` 以上留白。
- 容器最大宽度 `max-w-6xl`；左右内边距 `16px`（窄屏）/ `32px`（`sm`）/ `48px`（`lg`）。
- 列表和卡片使用直角、1px 边框；不使用阴影。
- 主按钮：实心赤陶橙、直角、`min-height: 44px`。
- 次按钮：透明底、1px 边框、直角。

## 6. 媒体与装饰

允许：

- 1px 细线、留白、大写等宽标签、衬线大标题。
- 代码块使用 1px 边框、`card` 底色。

禁止：

- 径向/线性渐变光晕、`blur` 光斑。
- 网格纹理（公开站不使用产品网格纹理）。
- 彩色阴影、`shadow-lg` 以上的阴影。
- `rounded-xl` / `rounded-2xl` / `rounded-full`（状态点与头像除外）。
- 玻璃拟态（`backdrop-blur` 大面积使用）。

## 7. 动效

- 只动 `opacity` 与不超过 `8px` 的位移。
- 时长 `150`–`300ms`。
- `prefers-reduced-motion: reduce` 时关闭位移，只保留或不使用透明度变化。

## 8. 实现位置

- `frontend/src/index.css`：`.public-surface` 与 `.dark .public-surface` token 覆盖；公开站阴影置零。
- `frontend/src/pages/public-layout.tsx`：根元素加 `public-surface`。
- `frontend/src/pages/welcome.tsx`：移除 hero 渐变/网格/光晕，放大衬线标题，代码块去阴影。
- 不确定项标记为 `[待确认]`，不写成事实。
