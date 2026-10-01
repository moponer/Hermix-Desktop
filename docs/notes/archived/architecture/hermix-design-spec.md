# Hermix 桌面版设计规范

Archived: 2026-09-30

## 一、配色方案

### 主色调
```
主色：#1A1A1A（近黑）
辅助：#333333（深灰）
点缀：#666666（中灰）
```

### 背景系统
```
层级    色值        用途
─────────────────────────────
一级    #FFFFFF     主内容区、卡片
二级    #F7F7F7     侧边栏、输入框
三级    #EFEFEF     分割线、边框
四级    #E5E5E5     hover 状态
五级    #D0D0D0     边框、分隔线
```

### 文字系统
```
层级    色值        用途
─────────────────────────────
一级    #1A1A1A     标题、主要文字
二级    #333333     正文
三级    #666666     次要信息
四级    #999999     占位符、禁用态
```

### 功能色
```
成功    #2E7D32
警告    #F57C00
错误    #D32F2F
信息    #1976D2
```

### 按钮样式
```
主按钮：
  默认：#1A1A1A 白字
  hover：#000000
  disabled：#E5E5E5 灰字

次按钮：
  默认：#FFFFFF 黑字 1px #D0D0D0 边框
  hover：#F7F7F7

图标按钮：
  默认：透明 灰图标
  hover：#F0F0F0
```

---

## 二、布局规范

### 窗口结构
```
┌─────────────────────────────────────────────────────┐
│  [H] Hermix                              [−□×]     │ ← 标题栏 32px
├──────────┬──────────────────────────────────────────┤
│          │  [☰] 瞎聊          [闲聊|协作]  [⚙]     │ ← 导航栏 44px
│  侧边栏   ├──────────────────────────────────────────┤
│  200px   │  邀请同事加入此房间开始协作 [邀请成员]    │ ← 工具栏 48px
│          ├──────────────────────────────────────────┤
│  • 群聊  │                                          │
│  • 对话  │              内容区域                     │
│  • 搜索  │           (自适应高度)                   │
│  • 应用  │                                          │
│          ├──────────────────────────────────────────┤
│  最近会话│  [📎 😊] 添加成员后可发送消息       [➤] │ ← 输入框 56px
│  测试... │  Enter 发送 · Shift+Enter 换行      0人 │
│  瞎聊    │                                          │
│          └──────────────────────────────────────────┘
└──────────┴──────────────────────────────────────────┘
```

### 间距系统
```
基准单位：4px

间距等级：
  xs:  4px   ← 图标与文字间距
  sm:  8px   ← 内边距小元素
  md:  12px  ← 常规间距
  lg:  16px  ← 区块间距
  xl:  24px  ← 大区块间距
  xxl: 32px  ← 页面边距
```

### 圆角规范
```
极小：2px   ← 标签、徽章
小：  4px   ← 按钮、输入框
中：  6px   ← 卡片、面板
大：  8px   ← 对话框、弹窗
```

### 字体系统
```
字体族：
  -apple-system, BlinkMacSystemFont, 'Segoe UI', 'Microsoft YaHei', sans-serif

字号：
  11px  ← 辅助文字、时间戳
  12px  ← 次要信息、标签
  13px  ← 正文、按钮文字
  14px  ← 标题、输入框
  16px  ← 大标题

字重：
  400   ← 正文
  500   ← 强调文字
  600   ← 标题
  700   ← 重要标题
```

---

## 三、组件规范

### 侧边栏
```
宽度：200px（展开）/ 48px（折叠）
高度：calc(100vh - 32px)
内边距：0 8px
项高度：32px
选中态：左侧 2px 黑色边框 + #EFEFEF 背景
hover：#F0F0F0 背景
分割线：#E5E5E5 1px
```

### 顶部导航栏
```
高度：44px
内边距：0 16px
背景：#FFFFFF
边框底部：1px #E0E0E0
图标按钮：28×28px，圆角 4px
模式切换：分段控件样式
```

### 空状态卡片
```
最大宽度：400px
内边距：40px 32px
背景：#FFFFFF
边框：1px #E5E5E5
圆角：8px
图标：48×48px，圆角 50%，边框 2px #D0D0D0
标题：14px 600
描述：12px 400 #666666
```

### 输入框
```
高度：56px
背景：#F7F7F7
边框：1px #D0D0D0
圆角：6px
focus：边框 #1A1A1A
内边距：8px 10px
```

### 按钮
```
主按钮：
  高度：32px
  内边距：0 16px
  背景：#1A1A1A
  文字：white 13px 500
  圆角：4px
  hover：背景 #000000

次按钮：
  背景：transparent
  边框：1px #D0D0D0
  文字：#1A1A1A
```

---

## 四、快捷参考

### CSS 变量
```css
:root {
  /* 色彩 */
  --color-bg-primary: #FFFFFF;
  --color-bg-secondary: #F7F7F7;
  --color-bg-tertiary: #EFEFEF;
  --color-bg-hover: #F0F0F0;
  
  --color-text-primary: #1A1A1A;
  --color-text-secondary: #333333;
  --color-text-tertiary: #666666;
  --color-text-disabled: #999999;
  
  --color-border: #E0E0E0;
  --color-border-light: #F0F0F0;
  
  --color-accent: #1A1A1A;
  --color-accent-hover: #000000;
  
  /* 尺寸 */
  --size-sidebar: 200px;
  --size-sidebar-collapsed: 48px;
  --size-header: 44px;
  --size-input: 56px;
  
  /* 间距 */
  --space-xs: 4px;
  --space-sm: 8px;
  --space-md: 12px;
  --space-lg: 16px;
  --space-xl: 24px;
  
  /* 圆角 */
  --radius-sm: 2px;
  --radius-md: 4px;
  --radius-lg: 6px;
  --radius-xl: 8px;
}
```

### 布局公式
```css
/* 整体布局 */
.app {
  display: flex;
  height: 100vh;
}

.sidebar {
  width: 200px;
  flex-shrink: 0;
}

.main {
  flex: 1;
  display: flex;
  flex-direction: column;
}

.content {
  flex: 1;
  overflow-y: auto;
  padding: 24px;
}

.input-area {
  height: 56px;
  border-top: 1px solid #E0E0E0;
}
```
