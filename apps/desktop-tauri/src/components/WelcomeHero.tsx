/**
 * 欢迎页字标：HERMIX 逐字上浮。
 *
 * 2026-10-27 三选一（逐字上浮 / 打字机 / 呼吸点），用户定了「逐字上浮」：
 * 最克制，静止时就是一张标题，不会和「正在加载」的真实状态混淆。
 *
 * 纯 CSS 动画，不依赖 JS 定时器；尊重 prefers-reduced-motion。
 */
const LETTERS = ["H", "E", "R", "M", "I", "X"];

export function WelcomeHero() {
  return (
    <div className="hero-wordmark" role="img" aria-label="Hermix">
      {LETTERS.map((c, i) => (
        <span key={i} className="hero-letter" style={{ animationDelay: `${i * 70}ms` }}>
          {c}
        </span>
      ))}
    </div>
  );
}
