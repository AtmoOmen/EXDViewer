#version 300 es
precision highp float;

in vec2 v_ground;
in float v_depth;

/// How far apart the fine lines stand, in the world's own units.
uniform float u_step;
/// The near and far planes, which the fade is worked out between.
uniform vec2 u_range;

out vec4 o_color;

const vec3 LINE = vec3(0.55);
const vec3 DECADE = vec3(0.78);
const vec3 X_AXIS = vec3(0.85, 0.35, 0.35);
const vec3 Z_AXIS = vec3(0.38, 0.55, 0.9);

/// How much of the pixel the nearest line of a grid of this spacing covers, measured in the
/// footprint the pixel has on the ground: a line stays a pixel wide however far off it stands.
float lines(float spacing) {
	vec2 at = v_ground / spacing;
	vec2 held = abs(fract(at - 0.5) - 0.5) / fwidth(at);
	return 1.0 - min(min(held.x, held.y), 1.0);
}

/// The same for the one line through the origin.
float axis(float at) {
	return 1.0 - min(abs(at) / fwidth(at), 1.0);
}

vec4 layer(vec3 color, float alpha) {
	return vec4(color * alpha, alpha);
}

/// Premultiplied, so one layer covers the next by what it left uncovered.
vec4 over(vec4 top, vec4 under) {
	return top + under * (1.0 - top.a);
}

void main() {
	float span = u_range.y - u_range.x;
	// Gone before the far plane and arrived after the near one, so what the grid stops at is this
	// rather than a clip plane cutting a line across the frame.
	float fade = smoothstep(u_range.x, u_range.x + 0.05 * span, v_depth)
		* (1.0 - smoothstep(u_range.x + 0.65 * span, u_range.x + 0.95 * span, v_depth));

	vec4 held = over(
		layer(X_AXIS, axis(v_ground.y) * 0.9),
		over(
			layer(Z_AXIS, axis(v_ground.x) * 0.9),
			over(
				layer(DECADE, lines(u_step * 10.0) * 0.7),
				layer(LINE, lines(u_step) * 0.45))));
	o_color = held * fade;
}
