export const SIZE = 16;

export const C = {
  OUT: 1,
  FUR: 2,
  RED: 3,
} as const;

export const PALETTE: string[] = ["", "#45301f", "#f2c25e", "#e84a3f"];

export type PetVisualState =
  | "sleeping"
  | "idle"
  | "working"
  | "waiting"
  | "done"
  | "error";

const HEAD = [
  "................",
  "................",
  "................",
  ".....OOOOOO.....",
  "...OOGGGGGGOO...",
  "..OGGGGGGGGGGO..",
  ".OGGOGOGGOGOGGO.",
  ".OGGOGGGGGGOGGO.",
  ".OGGOGGOOGGOGGO.",
  "..OOOGGGOGGOOO..",
  "....OGGGGGGO....",
  "....ORRRRRRO....",
  "................",
  "................",
  "................",
  "................",
] as const;

const COLOR_BY_CHAR: Record<string, number> = {
  O: C.OUT,
  G: C.FUR,
  R: C.RED,
};

export class PixelGrid {
  data = new Uint8Array(SIZE * SIZE);

  clear() {
    this.data.fill(0);
  }

  set(x: number, y: number, c: number) {
    x = Math.round(x);
    y = Math.round(y);
    if (x < 0 || x >= SIZE || y < 0 || y >= SIZE) return;
    this.data[y * SIZE + x] = c;
  }

  rect(x: number, y: number, w: number, h: number, c: number) {
    for (let j = 0; j < h; j++) {
      for (let i = 0; i < w; i++) this.set(x + i, y + j, c);
    }
  }
}

function drawHead(g: PixelGrid, dy: number) {
  HEAD.forEach((row, y) => {
    [...row].forEach((ch, x) => {
      const color = COLOR_BY_CHAR[ch];
      if (color) g.set(x, y + dy, color);
    });
  });
}

function closeEyes(g: PixelGrid, dy: number) {
  g.set(6, 6 + dy, C.FUR);
  g.set(9, 6 + dy, C.FUR);
  g.rect(5, 7 + dy, 2, 1, C.OUT);
  g.rect(9, 7 + dy, 2, 1, C.OUT);
}

function drawWorkingMarks(g: PixelGrid, frame: number) {
  for (let i = 0; i <= frame; i++) g.set(6 + i * 2, 1, C.OUT);
}

function drawCompletionMark(g: PixelGrid) {
  g.rect(7, 0, 2, 2, C.RED);
  g.rect(7, 3, 2, 1, C.RED);
}

export function drawPet(
  g: PixelGrid,
  state: PetVisualState,
  t: number,
  hasPendingCompletion = false
) {
  g.clear();

  const working = state === "working";
  const needsAttention =
    hasPendingCompletion || state === "done" || state === "waiting" || state === "error";
  const bob = working ? Math.floor(t / 280) % 2 : 0;
  const dy = needsAttention && !working ? 1 : bob;

  drawHead(g, dy);

  if (state === "sleeping") {
    closeEyes(g, dy);
    const zFrame = Math.floor(t / 700) % 2;
    g.set(13, 2 - zFrame, C.OUT);
    g.set(14, 1 - zFrame, C.OUT);
    g.set(13, 0 - zFrame, C.OUT);
  } else if (state === "idle" && t % 4200 > 3970) {
    closeEyes(g, dy);
  }

  if (working) {
    const frame = Math.floor(t / 360) % 3;
    drawWorkingMarks(g, frame);
  }

  if (needsAttention) drawCompletionMark(g);
}
