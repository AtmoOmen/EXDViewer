#version 300 es
precision highp float;
precision highp sampler2D;

in vec2 v_uv;

uniform sampler2D u_frame;
uniform sampler2D u_depth;

out vec4 fragColor;

void main() {
	// Nothing drew where the depth buffer still holds what it was cleared to, and those pixels
	// belong to egui rather than to the frame.
	if (texture(u_depth, v_uv).r >= 1.0) {
		discard;
	}
	fragColor = vec4(texture(u_frame, v_uv).rgb, 1.0);
}
