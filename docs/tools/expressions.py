"""Port of firmware/components/botstack/face/bloub.cpp drawing, to preview the 16 expressions."""
import math, re, sys
src = open(sys.argv[1]).read()
rows = re.findall(r'\{"(\w+)", \{([^{}]*), \{eye\(([^)]*)\), eye\(([^)]*)\)\}\}\}', src)
def nums(s): return [float(x.rstrip('f')) for x in s.split(',') if x.strip()]
W, H, R = 320, 240, 100.0
BG, DISC, EYE = (11, 11, 12), (240, 138, 36), (249, 249, 249)
def rot(e, t, n):
    c, s = math.cos(n), math.sin(n)
    return [e[k]*c + t[k]*s for k in range(3)], [t[k]*c - e[k]*s for k in range(3)]
def project(yaw, pitch, roll, split):
    rad = math.radians; f, r, u = [0,0,1], [1,0,0], [0,1,0]
    f, r = rot(f, r, rad(yaw)); u, f = rot(u, f, rad(pitch)); r, u = rot(r, u, rad(roll))
    out = []
    for sgn in (-1, 1):
        o, s = rot(f, r, rad(split*sgn)); out.append((o[0], o[1], s[0], s[1], u[0], u[1], o[2]))
    return out
def rr(x, y, hw, hh):
    rr_ = min(hw, hh); qx = abs(x)-hw+rr_; qy = abs(y)-hh+rr_
    return math.hypot(max(qx,0), max(qy,0)) + min(max(qx,qy),0) - rr_
def frame(pose, eyes):
    img = [[BG]*W for _ in range(H)]
    cx, cy = W//2, H//2
    for y in range(H):
        for x in range(W):
            cov = sum(((x+ox-cx+.5)**2+(y+oy-cy+.5)**2) <= R*R for ox in (-.25,.25) for oy in (-.25,.25))/4
            if cov: img[y][x] = tuple(round(BG[i]*(1-cov)+DISC[i]*cov) for i in range(3))
    yaw, pitch, roll, split = pose
    for (px, py, a0, b0, c0, d0, depth), ev in zip(project(yaw, pitch, roll, split), eyes):
        w, h, tilt, *op = ev + [0] * (3 - len(ev)) if len(ev) < 3 else ev
        if depth <= .02: continue
        alpha = min(1, depth/.12); t = math.radians(tilt); tc, ts = math.cos(t), math.sin(t)
        a = a0*tc + c0*ts; b = b0*tc + d0*ts; c = -a0*ts + c0*tc; d = -b0*ts + d0*tc
        openv = .06 + .94*min(1, op[0] if op else 1); b *= openv; d *= openv
        det = a*d - b*c; hw, hh = w*R/2, h*R/2
        ex, ey = abs(a)*hw + abs(c)*hh, abs(b)*hw + abs(d)*hh
        ecx, ecy = cx + px*R, cy + py*R
        sdf = lambda sx, sy: rr((d*sx - c*sy)/det, (-b*sx + a*sy)/det, hw, hh)
        for y in range(int(ecy-ey)-2, int(ecy+ey)+3):
            for x in range(int(ecx-ex)-2, int(ecx+ex)+3):
                sx, sy = x+.5-ecx, y+.5-ecy
                dist = sdf(sx, sy); g = math.hypot(sdf(sx+1, sy)-dist, sdf(sx, sy+1)-dist)
                cov = max(0, min(1, .5 - dist/max(g, 1e-4))) * alpha
                if cov and 0 <= x < W and 0 <= y < H:
                    p = img[y][x]; img[y][x] = tuple(round(p[i]*(1-cov)+EYE[i]*cov) for i in range(3))
    return img
for name, pose, e0, e1 in rows:
    img = frame(nums(pose), [nums(e0), nums(e1)])
    with open(f"{name}.ppm", "wb") as f:
        f.write(f"P6 {W} {H} 255\n".encode()); f.write(bytes(v for row in img for px in row for v in px))
    print(name, end=" ", flush=True)
print()
