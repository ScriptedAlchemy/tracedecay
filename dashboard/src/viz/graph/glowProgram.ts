import { NodeCircleProgram } from 'sigma/rendering';

/** Reuse Sigma's geometry, camera uniforms and lifecycle. Only decorative
 * companions have radial falloff; project bodies retain their exact hit area. */
export class GlowProgram extends NodeCircleProgram {
  override getDefinition() {
    return {
      ...super.getDefinition(),
      FRAGMENT_SHADER_SOURCE: `
precision highp float;
varying vec4 v_color;
varying vec2 v_diffVector;
varying float v_radius;
void main(void) {
  // Decoration must never intercept a body's pointer target.
  #ifdef PICKING_MODE
    discard;
  #else
    float radius = length(v_diffVector) / max(v_radius, 0.0001);
    float falloff = exp(-4.0 * radius * radius) * (1.0 - smoothstep(0.7, 1.0, radius));
    gl_FragColor = v_color * falloff;
  #endif
}`,
    };
  }
}
