import {
  ACESFilmicToneMapping,
  AdditiveBlending,
  AmbientLight,
  BoxGeometry,
  BufferAttribute,
  BufferGeometry,
  CanvasTexture,
  Clock,
  DirectionalLight,
  DoubleSide,
  Group,
  Mesh,
  MeshBasicMaterial,
  MeshPhysicalMaterial,
  PerspectiveCamera,
  PointLight,
  Points,
  PointsMaterial,
  PMREMGenerator,
  Scene,
  SRGBColorSpace,
  WebGLRenderer,
} from "three";
import { RoomEnvironment } from "three/examples/jsm/environments/RoomEnvironment.js";

const COLS = 340;
const ROWS = 12;
const WIDTH = 12.6;

const smoothstep = (edge0: number, edge1: number, value: number) => {
  const t = Math.min(1, Math.max(0, (value - edge0) / (edge1 - edge0)));
  return t * t * (3 - 2 * t);
};

function softDot(): CanvasTexture {
  const size = 64;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext("2d");
  if (context) {
    const gradient = context.createRadialGradient(size / 2, size / 2, 0, size / 2, size / 2, size / 2);
    gradient.addColorStop(0, "rgba(255,255,255,1)");
    gradient.addColorStop(0.45, "rgba(255,255,255,.65)");
    gradient.addColorStop(1, "rgba(255,255,255,0)");
    context.fillStyle = gradient;
    context.fillRect(0, 0, size, size);
  }
  const texture = new CanvasTexture(canvas);
  texture.needsUpdate = true;
  return texture;
}

export function startRibbon(canvas: HTMLCanvasElement, reducedMotion: boolean): () => void {
  let renderer: WebGLRenderer;
  try {
    renderer = new WebGLRenderer({
      canvas,
      alpha: true,
      antialias: true,
      powerPreference: "high-performance",
    });
  } catch {
    return () => undefined;
  }

  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  renderer.outputColorSpace = SRGBColorSpace;
  renderer.toneMapping = ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.12;

  const scene = new Scene();
  const camera = new PerspectiveCamera(38, 1, 0.1, 60);
  camera.position.set(0, 0.2, 7.8);
  camera.lookAt(0, 0, 0);

  const pmrem = new PMREMGenerator(renderer);
  const environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environment = environment;

  scene.add(new AmbientLight(0xffffff, 0.4));
  const key = new DirectionalLight(0xffffff, 2.6);
  key.position.set(5, 7, 7);
  scene.add(key);
  const rim = new DirectionalLight(0xb9c4ff, 1.5);
  rim.position.set(-7, -2, -6);
  scene.add(rim);
  const fill = new PointLight(0xffffff, 30, 26);
  fill.position.set(0, 4.5, 5.5);
  scene.add(fill);

  const group = new Group();
  group.position.set(1.15, 0.05, 0);
  group.rotation.set(0.16, -0.14, -0.07);
  scene.add(group);

  const vertexCount = (COLS + 1) * (ROWS + 1);
  const positions = new Float32Array(vertexCount * 3);
  const uvs = new Float32Array(vertexCount * 2);
  const indices: number[] = [];

  for (let row = 0; row < ROWS; row += 1) {
    for (let col = 0; col < COLS; col += 1) {
      const a = row * (COLS + 1) + col;
      const b = a + 1;
      const c = a + (COLS + 1);
      const d = c + 1;
      indices.push(a, c, b, b, c, d);
    }
  }

  for (let row = 0; row <= ROWS; row += 1) {
    for (let col = 0; col <= COLS; col += 1) {
      const index = row * (COLS + 1) + col;
      uvs[index * 2] = col / COLS;
      uvs[index * 2 + 1] = row / ROWS;
    }
  }

  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(positions, 3));
  geometry.setAttribute("uv", new BufferAttribute(uvs, 2));
  geometry.setIndex(indices);

  const material = new MeshPhysicalMaterial({
    color: 0xdedcd6,
    metalness: 1,
    roughness: 0.17,
    clearcoat: 1,
    clearcoatRoughness: 0.08,
    envMapIntensity: 1.5,
    side: DoubleSide,
    iridescence: 0.22,
    iridescenceIOR: 1.32,
    iridescenceThicknessRange: [120, 520],
  });

  const ribbon = new Mesh(geometry, material);
  group.add(ribbon);

  const caretMaterial = new MeshBasicMaterial({
    color: 0xffffff,
    transparent: true,
    opacity: 0.94,
  });
  const caret = new Mesh(new BoxGeometry(0.026, 0.52, 0.026), caretMaterial);
  caret.position.set(WIDTH / 2 + 0.43, 0, 0);
  group.add(caret);

  const particleCount = 420;
  const particlePositions = new Float32Array(particleCount * 3);
  const particleSpeeds = new Float32Array(particleCount);
  for (let index = 0; index < particleCount; index += 1) {
    particlePositions[index * 3] = (Math.random() - 0.5) * 19;
    particlePositions[index * 3 + 1] = (Math.random() - 0.5) * 7;
    particlePositions[index * 3 + 2] = (Math.random() - 0.5) * 7 - 1;
    particleSpeeds[index] = 0.05 + Math.random() * 0.12;
  }
  const particleGeometry = new BufferGeometry();
  particleGeometry.setAttribute("position", new BufferAttribute(particlePositions, 3));
  const dot = softDot();
  const particleMaterial = new PointsMaterial({
    color: 0xffffff,
    size: 0.045,
    map: dot,
    transparent: true,
    opacity: 0.34,
    depthWrite: false,
    blending: AdditiveBlending,
    sizeAttenuation: true,
  });
  const particles = new Points(particleGeometry, particleMaterial);
  group.add(particles);

  const updateRibbon = (time: number) => {
    for (let col = 0; col <= COLS; col += 1) {
      const u = col / COLS;
      const calm = smoothstep(0.48, 0.98, u);
      const amplitude = Math.pow(1 - calm, 1.12);
      const x = (u - 0.5) * WIDTH + 0.4;
      const center =
        (Math.sin(u * 9.5 + time * 0.8) * 0.62 +
          Math.sin(u * 4.3 - time * 0.58) * 0.34) *
        amplitude;
      const width = 0.16 + 1.36 * Math.pow(1 - u, 1.12);
      const twist = Math.sin(u * 7.6 - time * 0.74) * 0.95 * amplitude;

      for (let row = 0; row <= ROWS; row += 1) {
        const v = row / ROWS - 0.5;
        const z = v * width;
        const y =
          center +
          Math.sin(twist) * z * 0.86 +
          Math.cos(u * 19 + time * 1.05) * 0.032 * amplitude;
        const index = (col * (ROWS + 1) + row) * 3;
        positions[index] = x;
        positions[index + 1] = y;
        positions[index + 2] = z;
      }
    }
    geometry.attributes.position.needsUpdate = true;
    geometry.computeVertexNormals();
  };

  const updateParticles = (delta: number) => {
    const array = particleGeometry.attributes.position.array as Float32Array;
    for (let index = 0; index < particleCount; index += 1) {
      array[index * 3 + 1] += particleSpeeds[index] * delta;
      if (array[index * 3 + 1] > 3.6) {
        array[index * 3 + 1] = -3.6;
      }
    }
    particleGeometry.attributes.position.needsUpdate = true;
  };

  const pointer = { x: 0, y: 0 };
  const onPointerMove = (event: PointerEvent) => {
    pointer.x = (event.clientX / window.innerWidth) * 2 - 1;
    pointer.y = (event.clientY / window.innerHeight) * 2 - 1;
  };
  window.addEventListener("pointermove", onPointerMove, { passive: true });

  const fit = () => {
    const width = canvas.clientWidth || window.innerWidth;
    const height = canvas.clientHeight || window.innerHeight;
    renderer.setSize(width, height, false);
    camera.aspect = width / height;
    camera.position.z = width < 760 ? 10.8 : 7.8;
    camera.position.y = width < 760 ? 0.7 : 0.2;
    camera.lookAt(0, 0, 0);
    camera.updateProjectionMatrix();
    group.scale.setScalar(width < 760 ? 0.76 : 1);
  };
  fit();

  const resizeObserver = new ResizeObserver(fit);
  resizeObserver.observe(canvas);

  const clock = new Clock();
  let time = 1.4;
  let frameHandle = 0;
  let visible = true;

  const onVisibility = () => {
    visible = !document.hidden;
  };
  document.addEventListener("visibilitychange", onVisibility);

  const render = () => {
    frameHandle = window.requestAnimationFrame(render);
    if (!visible) {
      return;
    }
    const delta = Math.min(clock.getDelta(), 0.05);
    time += delta * 0.42;

    updateRibbon(time);
    updateParticles(delta);

    group.rotation.x += (0.16 + pointer.y * 0.075 - group.rotation.x) * 0.045;
    group.rotation.y += (-0.14 + pointer.x * 0.11 - group.rotation.y) * 0.045;

    const scroll = Math.min(window.scrollY / Math.max(window.innerHeight, 1), 1);
    group.position.y = 0.05 + scroll * 0.62;
    group.position.x = 1.15 - scroll * 0.28;
    group.rotation.z = -0.07 - scroll * 0.05;

    caretMaterial.opacity = Math.sin(time * 4.1) > 0 ? 0.94 : 0.04;
    renderer.render(scene, camera);
  };

  if (reducedMotion) {
    updateRibbon(time);
    renderer.render(scene, camera);
  } else {
    render();
  }

  canvas.classList.add("is-ready");

  return () => {
    window.cancelAnimationFrame(frameHandle);
    resizeObserver.disconnect();
    window.removeEventListener("pointermove", onPointerMove);
    document.removeEventListener("visibilitychange", onVisibility);
    geometry.dispose();
    material.dispose();
    caret.geometry.dispose();
    caretMaterial.dispose();
    particleGeometry.dispose();
    particleMaterial.dispose();
    dot.dispose();
    environment.dispose();
    pmrem.dispose();
    renderer.dispose();
  };
}
