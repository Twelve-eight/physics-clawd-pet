import { describe, expect, it } from 'vitest';
import { Kicker, resolveKick } from '../src/engine/kick';
import { DEFAULT_TUNING, type Tuning } from '../src/engine/params';
import { Pet } from '../src/engine/pet';

const STEP = 1 / 120;
const W = 1600;
const H = 900;
const rng = () => 0.5;

function makePet(over: Partial<Tuning> = {}) {
  return new Pet({ ...DEFAULT_TUNING, ...over }, [16, 10], { left: 0, top: 0, right: W, bottom: H }, rng);
}

const still = { x: 0, y: 0 };

describe('resolveKick', () => {
  const pet = { left: 400, top: 400, right: 520, bottom: 520 };

  it('没碰到就没有结果', () => {
    expect(resolveKick(pet, { left: 520, top: 400, right: 700, bottom: 520 }, still, DEFAULT_TUNING, 6500, rng)).toBeNull();
    expect(resolveKick(pet, { left: 900, top: 900, right: 1000, bottom: 1000 }, still, DEFAULT_TUNING, 6500, rng)).toBeNull();
  });

  it('不管从哪个方向撞，都往上踢，并且被推出窗口', () => {
    const sides = {
      左: { left: 300, top: 380, right: 410, bottom: 540 },
      右: { left: 510, top: 380, right: 700, bottom: 540 },
      下: { left: 380, top: 510, right: 540, bottom: 700 },
      上: { left: 380, top: 300, right: 540, bottom: 410 },
    };
    for (const [name, win] of Object.entries(sides)) {
      const r = resolveKick(pet, win, still, DEFAULT_TUNING, 6500, rng)!;
      expect(r, name).not.toBeNull();
      expect(r.vy, name).toBeLessThan(0);
      const moved = { left: pet.left + r.dx, right: pet.right + r.dx, top: pet.top + r.dy, bottom: pet.bottom + r.dy };
      const ox = Math.min(moved.right, win.right) - Math.max(moved.left, win.left);
      const oy = Math.min(moved.bottom, win.bottom) - Math.max(moved.top, win.top);
      expect(ox <= 1e-9 || oy <= 1e-9, `${name} 推完还在窗口里`).toBe(true);
    }
  });

  it('窗口越快踢得越高，但不超过上限', () => {
    const win = { left: 300, top: 380, right: 410, bottom: 540 };
    const slow = resolveKick(pet, win, still, DEFAULT_TUNING, 6500, rng)!;
    const fast = resolveKick(pet, win, { x: 1500, y: -800 }, DEFAULT_TUNING, 6500, rng)!;
    expect(fast.vy).toBeLessThan(slow.vy);
    expect(-fast.vy).toBeLessThanOrEqual(DEFAULT_TUNING.kickUpMax * 1.2);
  });
});

describe('Kicker', () => {
  const pet = { left: 400, top: 400, right: 520, bottom: 520 };
  const win = { left: 300, top: 380, right: 410, bottom: 540 };

  it('窗口停在宠物身上不连踢；移开再撞才再踢', () => {
    const k = new Kicker();
    const P = DEFAULT_TUNING;
    expect(k.update(0, pet, win, still, P, 6500, rng)!.kicked).toBe(true);
    // 同一个重叠，窗口不动：只推开
    expect(k.update(200, pet, win, still, P, 6500, rng)!.kicked).toBe(false);
    expect(k.update(400, pet, win, still, P, 6500, rng)!.kicked).toBe(false);
    // 快速移动的窗口持续压着：照踢
    expect(k.update(600, pet, win, { x: 800, y: 0 }, P, 6500, rng)!.kicked).toBe(true);
    // 离开再回来：新的接触
    expect(k.update(800, { ...pet, left: 900, right: 1020 }, win, still, P, 6500, rng)).toBeNull();
    expect(k.update(1000, pet, win, still, P, 6500, rng)!.kicked).toBe(true);
  });

  it('冷却时间内不重复踢', () => {
    const k = new Kicker();
    const P = DEFAULT_TUNING;
    expect(k.update(0, pet, win, { x: 800, y: 0 }, P, 6500, rng)!.kicked).toBe(true);
    expect(k.update(P.kickCooldownMs - 10, pet, win, { x: 800, y: 0 }, P, 6500, rng)!.kicked).toBe(false);
    expect(k.update(P.kickCooldownMs + 10, pet, win, { x: 800, y: 0 }, P, 6500, rng)!.kicked).toBe(true);
  });
});

describe('Pet.kickBy', () => {
  function settle(pet: Pet) {
    pet.placeOnFloor(800, H);
    for (let i = 0; i < 240; i++) pet.step(STEP, null);
  }
  const under = (pet: Pet) => {
    const b = pet.hitBox;
    return { left: b.left - 60, top: b.bottom - 30, right: b.right + 60, bottom: b.bottom + 60 };
  };

  it('站在地上被窗口撞一下就飞起来，落回下面后又弹起（不吸附）', () => {
    const pet = makePet();
    settle(pet);
    const floorY = pet.pos.y;
    expect(pet.kickBy(1000, under(pet), { x: 300, y: -400 })).toBe(true);
    expect(pet.mode).toBe('air');
    let apex = floorY;
    for (let i = 0; i < 3 * 120 && pet.mode === 'air'; i++) {
      pet.step(STEP, null);
      apex = Math.min(apex, pet.pos.y);
    }
    expect(floorY - apex).toBeGreaterThan(150);
    for (let i = 0; i < 6 * 120; i++) pet.step(STEP, null);
    expect(pet.pos.y).toBeGreaterThan(apex + 100);
  });

  it('不停地撞，它就一直在空中（连续踢十次，没有落回地面）', () => {
    const pet = makePet();
    settle(pet);
    const floorY = pet.pos.y;
    let t = 1000;
    let kicks = 0;
    let airborneAtEnd = true;
    for (let n = 0; n < 10; n++) {
      // 等它升到最高点再落回比地面高 60px 以内时，窗口从下面接一脚
      for (let i = 0; i < 6 * 120 && !(pet.vel.y > 0 && pet.pos.y > floorY - 60); i++) {
        pet.step(STEP, null);
        t += STEP * 1000;
      }
      if (pet.kickBy(t, under(pet), { x: 100, y: -600 })) kicks++;
      airborneAtEnd = pet.mode === 'air';
      // 窗口抽走：下一轮是新的接触
      pet.kickBy(t, null, still);
    }
    expect(kicks).toBeGreaterThanOrEqual(8);
    expect(airborneAtEnd).toBe(true);
  });

  it('被鼠标拎着的时候不踢；关掉踢毽子也不踢', () => {
    const pet = makePet();
    settle(pet);
    pet.grab(pet.pos.x, pet.pos.y);
    expect(pet.kickBy(1000, under(pet), { x: 800, y: -400 })).toBe(false);
    expect(pet.mode).toBe('held');

    const off = makePet({ kickEnabled: 0 });
    settle(off);
    expect(off.kickBy(1000, under(off), { x: 800, y: -400 })).toBe(false);
    expect(off.mode).not.toBe('air');
  });

  const stepFor = (pet: Pet, seconds: number, each?: (p: Pet) => void) => {
    for (let i = 0; i < seconds / STEP; i++) {
      pet.step(STEP, null);
      each?.(pet);
    }
  };

  it('每次触底都损失能量，越弹越矮；弹不起来时毽子模式随之结束并正常落地', () => {
    const pet = makePet();
    settle(pet);
    const floorY = pet.pos.y;
    pet.kickBy(1000, under(pet), { x: 0, y: -400 });
    expect(pet.juggling).toBe(true);
    const peaks: number[] = [];
    let apex = pet.pos.y;
    let prevVy = pet.vel.y;
    let endedAt = -1;
    let i = 0;
    stepFor(pet, 30, (p) => {
      i++;
      apex = Math.min(apex, p.pos.y);
      if (prevVy > 0 && p.vel.y <= 0) {
        peaks.push(floorY - apex);
        apex = p.pos.y;
      }
      prevVy = p.vel.y;
      if (endedAt < 0 && !p.juggling) endedAt = i;
    });
    expect(peaks.length).toBeGreaterThanOrEqual(3);
    for (let k = 1; k < peaks.length; k++) expect(peaks[k]).toBeLessThan(peaks[k - 1]);
    expect(endedAt).toBeGreaterThan(0);
    // 结束之后是普通落地：站到屏幕底边上，不再是 air
    expect(pet.mode).not.toBe('air');
    expect(pet.juggling).toBe(false);
    expect(Math.abs(pet.pos.y - floorY)).toBeLessThan(5);
  });

  it('被踢之后经过窗口顶边也不会站上去', () => {
    const pet = makePet();
    settle(pet);
    const floorY = pet.pos.y + 0;
    pet.setPlatforms([{ id: 7, x0: 0, x1: W, y: floorY - 150 }]);
    pet.kickBy(1000, under(pet), { x: 0, y: -400 });
    const modes = new Set<string>();
    let onPlatform = false;
    stepFor(pet, 20, (p) => {
      if (!p.juggling) return;
      modes.add(p.mode);
      if (p.support !== null) onPlatform = true;
    });
    expect([...modes]).toEqual(['air']);
    expect(onPlatform).toBe(false);
  });

  it('撞到左右屏幕边框只反弹，不贴墙', () => {
    const pet = makePet();
    settle(pet);
    pet.kickBy(1000, under(pet), { x: 0, y: -400 });
    pet.vel = { x: -3000, y: -300 };
    const modes = new Set<string>();
    let flipped = false;
    stepFor(pet, 3, (p) => {
      if (p.juggling) modes.add(p.mode);
      if (p.vel.x > 0) flipped = true;
    });
    expect(flipped).toBe(true);
    expect(modes.has('cling')).toBe(false);
  });

  it('用户抓一下（点击或长按）就解除，之后正常落地', () => {
    const pet = makePet();
    settle(pet);
    pet.kickBy(1000, under(pet), { x: 300, y: -400 });
    stepFor(pet, 1);
    pet.grab(pet.pos.x, pet.pos.y);
    expect(pet.juggling).toBe(false);
    pet.release(0, 0, true);
    stepFor(pet, 20);
    expect(pet.mode).not.toBe('air');
    expect(pet.pos.y).toBeGreaterThan(H - 200);
  });

  it('毽子飞行中 agent 状态变化或提醒动作都不会把它拽下来', () => {
    const pet = makePet();
    settle(pet);
    pet.kickBy(1000, under(pet), { x: 300, y: -400 });
    const modes = new Set<string>();
    let t = 0;
    stepFor(pet, 30, (p) => {
      if (p.juggling) modes.add(p.mode);
      t += STEP;
      if (Math.abs(t - 1) < STEP / 2) p.setAgentMood('working');
      if (Math.abs(t - 3) < STEP / 2) p.cue('zap');
      if (Math.abs(t - 5) < STEP / 2) p.cue('celebrate');
      if (Math.abs(t - 7) < STEP / 2) p.setAgentMood('compacting');
    });
    expect([...modes]).toEqual(['air']);
  });
});
