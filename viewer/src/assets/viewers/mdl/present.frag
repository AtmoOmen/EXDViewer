#version 300 es
precision highp float;
precision highp sampler2D;

in vec2 v_uv;

uniform sampler2D u_frame;
uniform sampler2D u_depth;
uniform bool u_tone;

out vec4 fragColor;

/// Where the frame stops being taken as it arrived and starts being bent toward what a screen can
/// hold. Both this and where the bend settles are the viewer's own: nothing on disk states the
/// exposure the game works out a frame at a time.
const float KNEE = 0.8;

float shoulder(float value) {
	return value <= KNEE
		? value
		: KNEE + (1.0 - KNEE) * (1.0 - exp((KNEE - value) / (1.0 - KNEE)));
}

void main() {
	// Nothing drew where the depth buffer still holds what it was cleared to, and those pixels
	// belong to egui rather than to the frame.
	float depth = texture(u_depth, v_uv).r;
	if (depth >= 1.0) {
		discard;
	}
	// Carried over so what is drawn on top of the frame can test against what it covered. It only
	// lands where the caller left depth writes on, which the pass that grades a frame does not.
	gl_FragDepth = depth;
	vec3 color = texture(u_frame, v_uv).rgb;
	if (u_tone) {
		// Every channel by what the brightest of them was bent by, so a pixel past the knee loses
		// brightness and not hue. A channel of its own would take the color with it.
		float peak = max(color.r, max(color.g, color.b));
		color *= peak > 0.0 ? shoulder(peak) / peak : 0.0;
	}
	fragColor = vec4(color, 1.0);
}
