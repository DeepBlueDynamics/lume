import React, { useMemo, useState, useEffect, useRef } from "react";
import { Canvas, useFrame } from "@react-three/fiber";
import { OrbitControls, Line, Html, Billboard, Text } from "@react-three/drei";
import * as THREE from "three";
import { qHsl, OVERLAP, nodeColors, colorOfNode } from "./colors.js";

const fmtWeight = (n) => (Math.abs(n) >= 100 ? n.toFixed(0) : n.toFixed(2));

const UP = new THREE.Vector3(0, 1, 0);
const _v = new THREE.Vector3();
const _q = new THREE.Quaternion();
const EMPTY = new Set();

// --- Hyperspace warp-in ---
// On each new field the orbs jump in: they start flung far out along their radial
// direction (stretched into thin streaks) and decelerate hard into place.
const WARP_DUR = 0.85;      // seconds per orb
const WARP_STAGGER = 0.30;  // max extra delay, spread across orbs for a cascade
const easeOutExpo = (p) => (p >= 1 ? 1 : 1 - Math.pow(2, -10 * p));
// Deterministic 0..1 hash from an integer id (for per-orb stagger / fallback dir).
const hash01 = (n) => { const x = Math.sin((n + 1) * 12.9898) * 43758.5453; return x - Math.floor(x); };

// Sets the shared warp start-time the first frame after a new field arrives.
// Registered before the orbs so their useFrame reads a fresh t0 the same frame.
function WarpClock({ warpRef }) {
  useFrame(({ clock }) => {
    if (warpRef.current.pending) { warpRef.current.t0 = clock.elapsedTime; warpRef.current.pending = false; }
  });
  return null;
}

// One white radial-gradient texture, tinted per-halo via the material colour.
function radialTexture() {
  if (typeof document === "undefined") return null;
  const s = 128, c = document.createElement("canvas");
  c.width = c.height = s;
  const ctx = c.getContext("2d");
  const g = ctx.createRadialGradient(s / 2, s / 2, 0, s / 2, s / 2, s / 2);
  g.addColorStop(0, "rgba(255,255,255,0.95)");
  g.addColorStop(0.35, "rgba(255,255,255,0.4)");
  g.addColorStop(1, "rgba(255,255,255,0)");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, s, s);
  return new THREE.CanvasTexture(c);
}
const HALO_TEX = radialTexture();

function Halo({ pos, r, color, k = 1 }) {
  return (
    <Billboard position={pos}>
      <mesh>
        <planeGeometry args={[r * 6, r * 6]} />
        <meshBasicMaterial map={HALO_TEX} color={color} transparent opacity={0.75 * k}
          blending={THREE.AdditiveBlending} depthWrite={false} />
      </mesh>
    </Billboard>
  );
}

function Node({ node, color, halo, haloK, accScale, warp, onHover, warpRef, center }) {
  const pos = node.pos;
  const isQ = node.is_query;
  const r = node.r ?? (isQ ? 0.3 : 0.2);

  const grpRef = useRef();
  const meshRef = useRef();

  // Where this orb starts its hyperspace jump: far out along its radial line
  // from the field centre (random direction if it sits dead-centre), plus its
  // stagger delay and the unit radial used to orient the streak.
  const warpIn = useMemo(() => {
    let dx = pos[0] - center[0], dy = pos[1] - center[1], dz = pos[2] - center[2];
    let m = Math.hypot(dx, dy, dz);
    if (m < 1e-3) { // dead-centre: pick a stable pseudo-random direction
      const a = hash01(node.id) * Math.PI * 2, b = hash01(node.id + 7) * Math.PI - Math.PI / 2;
      dx = Math.cos(a) * Math.cos(b); dy = Math.sin(b); dz = Math.sin(a) * Math.cos(b); m = 1;
    }
    const nx = dx / m, ny = dy / m, nz = dz / m;
    const dist = 12 + m * 5;
    const quat = new THREE.Quaternion().setFromUnitVectors(UP, _v.set(nx, ny, nz));
    return { off: [nx * dist, ny * dist, nz * dist], quat, delay: hash01(node.id) * WARP_STAGGER };
  }, [pos[0], pos[1], pos[2], center[0], center[1], center[2], node.id]);

  useFrame(({ clock }) => {
    const g = grpRef.current;
    if (!g) return;
    const t0 = warpRef.current.t0;
    let p = t0 <= -100 ? 1 : (clock.elapsedTime - t0 - warpIn.delay) / WARP_DUR;
    p = p < 0 ? 0 : p > 1 ? 1 : p;
    const k = 1 - easeOutExpo(p); // 1 → 0 over the jump
    g.position.set(warpIn.off[0] * k, warpIn.off[1] * k, warpIn.off[2] * k);
    if (p < 1 && meshRef.current) {
      // Streak: stretch along the radial travel axis, thinning out the sides.
      meshRef.current.quaternion.copy(warpIn.quat);
      meshRef.current.scale.set(1 / (1 + 2.2 * k), 1 + 7 * k, 1 / (1 + 2.2 * k));
    }
  });

  // Warp the orb to show motion through the vector space: stretch into an
  // ellipsoid along velocity (with an acceleration pulse).
  const { quat, scale } = useMemo(() => {
    const vel = node.vel || [0, 0, 0];
    const speed = Math.hypot(vel[0], vel[1], vel[2]);
    const accMag = Math.hypot(node.acc[0], node.acc[1], node.acc[2]);
    const s = Math.min(1.6, (speed + 0.5 * accMag) * warp);
    let quat = [0, 0, 0, 1];
    if (speed > 1e-7) {
      _v.set(vel[0], vel[1], vel[2]).normalize();
      _q.setFromUnitVectors(UP, _v);
      quat = [_q.x, _q.y, _q.z, _q.w];
    }
    return { quat, scale: [1 - 0.35 * s, 1 + s, 1 - 0.35 * s] };
  }, [node.vel, node.acc, warp]);

  const accEnd = useMemo(() => {
    const a = node.acc;
    const mag = Math.hypot(a[0], a[1], a[2]);
    if (mag < 1e-9) return null;
    const len = Math.min(4, mag * accScale);
    const k = len / mag;
    return [pos[0] + a[0] * k, pos[1] + a[1] * k, pos[2] + a[2] * k];
  }, [node.acc, pos, accScale]);

  return (
    <group ref={grpRef}>
      {halo && <Halo pos={pos} r={r} color={halo} k={haloK ?? (isQ ? 1 : 0.85)} />}
      <mesh
        ref={meshRef}
        position={pos}
        quaternion={isQ ? [0, 0, 0, 1] : quat}
        scale={isQ ? [1, 1, 1] : scale}
        onPointerOver={(e) => { e.stopPropagation(); onHover(node); }}
        onPointerOut={() => onHover(null)}
      >
        <sphereGeometry args={[r, 32, 32]} />
        <meshPhysicalMaterial color={color} emissive={color}
          emissiveIntensity={isQ ? 0.7 : 0.18 + 0.5 * Math.max(0, node.cos_q)}
          roughness={0.32} metalness={0.25} clearcoat={0.6} clearcoatRoughness={0.3}
          transparent={isQ} opacity={isQ ? 0.34 : 1} />
      </mesh>

      {accEnd && !isQ && (
        <Line points={[pos, accEnd]} color={color} lineWidth={node.approach_acc < 0 ? 2.4 : 1.1} />
      )}

      <Billboard position={[pos[0], pos[1] + r + 0.14, pos[2]]}>
        <Text fontSize={isQ ? 0.18 : 0.13} color={isQ ? "#ffffff" : color}
          anchorX="center" anchorY="bottom" outlineWidth={0.01} outlineColor="#05060a">
          {isQ ? "◆" : fmtWeight(node.score)}
        </Text>
      </Billboard>
    </group>
  );
}

function CtrlPanControls() {
  const controls = useRef();
  useEffect(() => {
    const onDown = (e) => { if (e.key === "Control" && controls.current) controls.current.mouseButtons.LEFT = THREE.MOUSE.PAN; };
    const onUp = (e) => { if (e.key === "Control" && controls.current) controls.current.mouseButtons.LEFT = THREE.MOUSE.ROTATE; };
    window.addEventListener("keydown", onDown);
    window.addEventListener("keyup", onUp);
    return () => { window.removeEventListener("keydown", onDown); window.removeEventListener("keyup", onUp); };
  }, []);
  return (
    <OrbitControls ref={controls} makeDefault enablePan enableDamping dampingFactor={0.08}
      screenSpacePanning
      mouseButtons={{ LEFT: THREE.MOUSE.ROTATE, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.PAN }} />
  );
}

function Tooltip({ node, color }) {
  if (!node) return null;
  const acc = node.approach_acc < 0 ? "#36d399" : "#ff5b6e";
  return (
    <Html position={node.pos} distanceFactor={10} zIndexRange={[100, 0]} style={{ pointerEvents: "none" }}>
      <div style={{
        transform: "translate(14px, -50%)", width: 280, background: "rgba(10,12,20,0.94)",
        border: `1px solid ${color}`, borderRadius: 8, padding: "8px 10px", color: "#e6e8ef",
        fontSize: 11, lineHeight: 1.45, fontFamily: "ui-monospace, Menlo, Consolas, monospace",
      }}>
        <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 4, color: "#9aa1b8" }}>
          <span>weight <b style={{ color: "#fff" }}>{node.is_query ? "—" : fmtWeight(node.score)}</b></span>
          <span>cos<sub>q</sub> <b style={{ color: "#fff" }}>{node.cos_q.toFixed(3)}</b></span>
          <span style={{ color: acc }}>d̈ {node.approach_acc >= 0 ? "+" : ""}{node.approach_acc.toFixed(3)}</span>
        </div>
        {node.members && node.members.length > 1 && (
          <div style={{ color: OVERLAP, marginBottom: 3 }}>★ overlap · queries {node.members.map((q) => q + 1).join(" + ")}</div>
        )}
        <div style={{ color: "#cfd6ee" }}>{node.text || node.label}</div>
      </div>
    </Html>
  );
}

export default function VectorField({ nodes, accScale, warp, queryCount, hoveredId, onHover, usedIds, citedIds, warpKey }) {
  const multi = (queryCount || 1) > 1;
  const used = usedIds || EMPTY, cited = citedIds || EMPTY;

  // Warp-in clock: arm a fresh start time whenever a new field arrives.
  const warpRef = useRef({ t0: -999, pending: false });
  useEffect(() => { warpRef.current.pending = true; }, [warpKey]);

  // Field centre the orbs jump in toward (mean of current positions).
  const center = useMemo(() => {
    if (!nodes.length) return [0, 0, 0];
    let x = 0, y = 0, z = 0;
    for (const n of nodes) { x += n.pos[0]; y += n.pos[1]; z += n.pos[2]; }
    return [x / nodes.length, y / nodes.length, z / nodes.length];
  }, [nodes]);

  // Shared colour logic (see colors.js) so list + orbs match.
  const colorById = useMemo(() => nodeColors(nodes, multi), [nodes, multi]);
  const colorOf = (nd) => colorOfNode(nd, colorById, multi);
  // Halo priority: hover > answer citation > query > overlap > answer "used".
  const haloOf = (nd) => {
    if (nd.id === hoveredId) return "#ffffff";
    if (cited.has(nd.id)) return "#9be7ff";           // cited by the answer — brightest
    if (nd.is_query) return multi ? qHsl(nd.query_index, 66) : "#9fb4ff";
    if (nd.members && nd.members.length > 1) return OVERLAP;
    if (used.has(nd.id)) return "#46506e";            // considered for the answer — soft
    return null;
  };
  const haloK = (nd) => (cited.has(nd.id) ? 1.3 : used.has(nd.id) && !nd.is_query ? 0.5 : 1);
  const hoveredLive = hoveredId != null ? nodes.find((n) => n.id === hoveredId) : null;

  return (
    <Canvas camera={{ position: [0, 2, 16], fov: 50 }} dpr={[1, 2]}>
      <color attach="background" args={["#05060a"]} />
      <ambientLight intensity={0.4} />
      <directionalLight position={[6, 10, 8]} intensity={2.6} />
      <directionalLight position={[-8, 4, -6]} intensity={0.8} color="#6f8cff" />
      <pointLight position={[-10, -6, -8]} intensity={50} color="#3344ff" />
      <gridHelper args={[50, 50, "#10131f", "#0a0c14"]} position={[0, -7, 0]} />

      <WarpClock warpRef={warpRef} />
      {nodes.map((n) => (
        <Node key={n.id} node={n} color={colorOf(n)} halo={haloOf(n)} haloK={haloK(n)} accScale={accScale} warp={warp}
          onHover={(node) => onHover(node ? node.id : null)} warpRef={warpRef} center={center} />
      ))}
      <Tooltip node={hoveredLive} color={hoveredLive ? colorOf(hoveredLive) : "#888"} />

      <CtrlPanControls />
    </Canvas>
  );
}
