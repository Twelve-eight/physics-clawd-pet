// 踢毽子：用户拖着的窗口撞到宠物，把它往上踢；一直撞就一直在空中。
// 纯函数，不碰 Pet 的内部状态；Pet.kickBy 用它的结果起飞。

import type { Vec2 } from './math';
import type { Bounds } from './world';

export interface KickParams {
  /** 最低向上速度 (px/s) */
  kickUpMin: number;
  /** 向上速度上限 (px/s) */
  kickUpMax: number;
  /** 窗口越快踢得越高：窗口速度 × 这个系数加到向上速度上 */
  kickUpGain: number;
  /** 基础侧推 (px/s)，方向是远离窗口中心 */
  kickSide: number;
  /** 窗口自己的水平速度传给宠物的比例 */
  kickCarry: number;
  /** 侧向随机范围 (px/s，总宽度) */
  kickJitter: number;
  /** 两次踢之间的最短间隔 (ms) */
  kickCooldownMs: number;
  /** 低于这个窗口速度 (px/s) 的重叠只把宠物推开，不再补一脚 */
  kickMinSpeed: number;
}

export interface KickResult {
  /** 把宠物推出窗口需要的位移 */
  dx: number;
  dy: number;
  vx: number;
  vy: number;
}

function overlapOf(a: Bounds, b: Bounds): { ox: number; oy: number } | null {
  const ox = Math.min(a.right, b.right) - Math.max(a.left, b.left);
  const oy = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
  return ox > 0 && oy > 0 ? { ox, oy } : null;
}

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/**
 * 宠物碰撞盒 pet 和窗口 win 重叠时的结果；没重叠返回 null。
 * 结果的 vy 一定是向上的（负）。rng 返回 [0,1)。
 */
export function resolveKick(
  pet: Bounds,
  win: Bounds,
  winVel: Vec2,
  P: KickParams,
  maxSpeed: number,
  rng: () => number = Math.random,
): KickResult | null {
  const o = overlapOf(pet, win);
  if (!o) return null;
  const wvx = Number.isFinite(winVel.x) ? winVel.x : 0;
  const wvy = Number.isFinite(winVel.y) ? winVel.y : 0;
  const petCx = (pet.left + pet.right) / 2;
  const petCy = (pet.top + pet.bottom) / 2;
  const winCx = (win.left + win.right) / 2;
  const winCy = (win.top + win.bottom) / 2;
  const dir = petCx >= winCx ? 1 : -1;
  const speed = Math.hypot(wvx, wvy);
  const up = clamp(P.kickUpMin + speed * P.kickUpGain, P.kickUpMin, P.kickUpMax);
  let vx = dir * P.kickSide + wvx * P.kickCarry + (rng() - 0.5) * P.kickJitter;
  let vy = -up - Math.max(0, -wvy) * 0.5;
  let dx = 0;
  let dy = 0;
  if (o.ox <= o.oy) {
    dx = dir * o.ox;
  } else if (petCy < winCy) {
    dy = -o.oy; // 宠物在窗口上沿：从上面顶出去
  } else {
    // 宠物在窗口下面：往下推开，侧向甩出去，免得上抛的弧线下一帧又进同一个窗口
    dy = o.oy;
    vy = -up * 0.6;
    vx = dir * Math.max(Math.abs(vx), P.kickSide * 2);
  }
  const lim = Math.min(maxSpeed, P.kickUpMax * 1.2);
  return { dx, dy, vx: clamp(vx, -maxSpeed, maxSpeed), vy: clamp(vy, -lim, lim) };
}

/**
 * 踢球手：记着上一帧有没有碰到，决定这一帧要不要补一脚。
 * 窗口停在宠物身上不动（速度低于 kickMinSpeed）时，只推开，不连踢。
 */
export class Kicker {
  private touching = false;
  private lastKickAt = -Infinity;

  reset() {
    this.touching = false;
  }

  /** 返回要应用的结果；kicked 为真时才改速度，否则只用 dx/dy 把宠物推出去 */
  update(
    now: number,
    pet: Bounds,
    win: Bounds,
    winVel: Vec2,
    P: KickParams,
    maxSpeed: number,
    rng?: () => number,
  ): (KickResult & { kicked: boolean }) | null {
    const r = resolveKick(pet, win, winVel, P, maxSpeed, rng);
    const was = this.touching;
    this.touching = r !== null;
    if (!r) return null;
    const fresh = !was || Math.hypot(winVel.x, winVel.y) >= P.kickMinSpeed;
    const kicked = fresh && now - this.lastKickAt >= P.kickCooldownMs;
    if (kicked) this.lastKickAt = now;
    return { ...r, kicked };
  }
}
