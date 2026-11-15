import {Component, createRef} from "react";
import {Button, Tag} from "react-bulma-components";

type PoseKeypoint = {
    name: string;
    x: number;
    y: number;
    score: number;
};

type PoseEstimate = {
    width: number;
    height: number;
    keypoints: PoseKeypoint[];
    inferenceMs: number;
    maxFps: number;
    provider: string;
};

type PoseStats = {
    inferenceMs: number;
    maxFps: number;
    provider: string;
};

type Resolution = {
    width: number;
    height: number;
};

const MIN_KEYPOINT_SCORE = 0.3;

// Skeleton dot radius at the reference resolution; scales proportionally
// to the frame width (the canvas stretches to the container width).
const KEYPOINT_RADIUS = 10;
const KEYPOINT_REFERENCE_WIDTH = 1024;

// Skeleton bones: pairs of keypoint indices (MoveNet/COCO-17 order) connected
// by lines.
const POSE_EDGES: ReadonlyArray<readonly [number, number]> = [
    [1, 3], [2, 4],        // eyes ↔ ears
    [5, 6],                // shoulders
    [5, 7], [7, 9],        // left arm
    [6, 8], [8, 10],       // right arm
    [11, 12],              // hips
    [11, 13], [13, 15],    // left leg
    [12, 14], [14, 16],    // right leg
];

// Leg keypoint indices (knees and ankles).
const LEG_KEYPOINT_INDICES = [13, 14, 15, 16];

// When the pelvis sits this close to the frame's bottom border (as a ratio of
// the frame height), knee/ankle points landing above it are considered
// hallucinations and the legs are hidden.
const PELVIS_BOTTOM_MARGIN_RATIO = 0.85;

// Bone line width at the reference resolution; scales like the dot radius.
const SKELETON_LINE_WIDTH = 6;

// Sliding-average window for recognition time and FPS on the badge.
const SMOOTHING_WINDOW = 10;
const FPS_SMOOTHING_WINDOW = 20;

// Frame header: width, height and JSON tail length with the recognized pose (u32 big-endian).
const FRAME_HEADER_BYTES = 12;

type CameraPreviewProps = {
    isStreaming: boolean;
    isStoppingCamera: boolean;
    onStop: () => void;
};

type CameraPreviewState = {
    resolution: Resolution | null;
    poseStats: PoseStats | null;
    mirror: boolean;
};

/**
 * Renders the camera stream: keeps only the latest frame, paints it on the
 * next animation frame, overlays the recognized keypoints and shows the
 * telemetry badges (resolution, recognition time/FPS, execution provider).
 *
 * The parent feeds frames through `pushFrame()` and calls `clear()` when the
 * stream stops.
 */
// Midpoint of two keypoints, or `null` when either point is missing or too
// uncertain.
function midpoint(a?: PoseKeypoint, b?: PoseKeypoint): {x: number; y: number} | null {
    if (!a || !b || a.score < MIN_KEYPOINT_SCORE || b.score < MIN_KEYPOINT_SCORE) {
        return null;
    }

    return {x: (a.x + b.x) / 2, y: (a.y + b.y) / 2};
}

export class CameraPreview extends Component<CameraPreviewProps, CameraPreviewState> {
    state: CameraPreviewState = {
        resolution: null,
        poseStats: null,
        mirror: true,
    };

    private readonly canvasRef = createRef<HTMLCanvasElement>();
    private imageData: ImageData | null = null;
    private pendingFrame: ArrayBuffer | null = null;
    private frameRequest: number | null = null;
    private latestPose: PoseEstimate | null = null;
    private inferenceMsWindow: number[] = [];
    private maxFpsWindow: number[] = [];

    // Called by the parent for every frame received from the camera channel.
    pushFrame(buffer: ArrayBuffer) {
        // Keep only the latest frame; draw it on the next animation frame.
        this.pendingFrame = buffer;
        if (this.frameRequest === null) {
            this.frameRequest = requestAnimationFrame(this.drawPendingFrame);
        }
    }

    // Resets the preview when the stream stops or fails.
    clear() {
        if (this.frameRequest !== null) {
            cancelAnimationFrame(this.frameRequest);
            this.frameRequest = null;
        }

        this.pendingFrame = null;
        this.imageData = null;
        this.latestPose = null;
        this.resetPoseWindows();
        this.setState({resolution: null, poseStats: null});

        const canvas = this.canvasRef.current;
        const context = canvas?.getContext("2d");
        if (canvas && context) {
            context.clearRect(0, 0, canvas.width, canvas.height);
        }
    }

    componentWillUnmount() {
        if (this.frameRequest !== null) {
            cancelAnimationFrame(this.frameRequest);
            this.frameRequest = null;
        }
    }

    private drawPendingFrame = () => {
        this.frameRequest = null;

        const frame = this.pendingFrame;
        this.pendingFrame = null;

        if (frame) {
            this.drawFrame(frame);
        }
    };

    private drawFrame(buffer: ArrayBuffer) {
        const canvas = this.canvasRef.current;
        if (!canvas || buffer.byteLength < FRAME_HEADER_BYTES) {
            return;
        }

        const header = new DataView(buffer);
        const width = header.getUint32(0, false);
        const height = header.getUint32(4, false);
        const poseLength = header.getUint32(8, false);
        const rgbLength = width * height * 3;
        const poseOffset = FRAME_HEADER_BYTES + rgbLength;

        if (width === 0 || height === 0 || buffer.byteLength < poseOffset + poseLength) {
            return;
        }

        const rgb = new Uint8Array(buffer, FRAME_HEADER_BYTES, rgbLength);

        // The resolution arrives in every frame header; redraw the badge only
        // when it actually changed.
        const resolution = this.state.resolution;
        if (!resolution || resolution.width !== width || resolution.height !== height) {
            this.setState({resolution: {width, height}});
        }

        if (canvas.width !== width || canvas.height !== height) {
            canvas.width = width;
            canvas.height = height;
            this.imageData = null;
        }

        const context = canvas.getContext("2d");
        if (!context) {
            return;
        }

        if (!this.imageData) {
            this.imageData = context.createImageData(width, height);
        }

        // The camera sends RGB, but ImageData needs RGBA, so expand the pixels.
        const rgba = this.imageData.data;
        for (let src = 0, dst = 0; dst < rgba.length; src += 3, dst += 4) {
            rgba[dst] = rgb[src];
            rgba[dst + 1] = rgb[src + 1];
            rgba[dst + 2] = rgb[src + 2];
            rgba[dst + 3] = 255;
        }

        this.applyPose(buffer, poseOffset, poseLength);
        context.putImageData(this.imageData, 0, 0);
        this.drawPose(context, width, height);
    }

    private applyPose(buffer: ArrayBuffer, offset: number, length: number) {
        if (length === 0) {
            this.latestPose = null;
            this.resetPoseWindows();
            this.setState({poseStats: null});
            return;
        }

        try {
            const json = new TextDecoder().decode(new Uint8Array(buffer, offset, length));
            const pose = JSON.parse(json) as PoseEstimate;
            this.latestPose = pose;
            this.setState({
                poseStats: {
                    inferenceMs: this.pushSmoothed(this.inferenceMsWindow, pose.inferenceMs, SMOOTHING_WINDOW),
                    maxFps: this.pushSmoothed(this.maxFpsWindow, pose.maxFps, FPS_SMOOTHING_WINDOW),
                    provider: pose.provider,
                },
            });
        } catch {
            // A corrupted JSON tail is simply ignored.
        }
    }

    // Keeps a window of the last `size` values and returns the average.
    private pushSmoothed(window: number[], value: number, size: number): number {
        window.push(value);
        if (window.length > size) {
            window.shift();
        }

        const sum = window.reduce((acc, item) => acc + item, 0);
        return sum / window.length;
    }

    private resetPoseWindows() {
        this.inferenceMsWindow = [];
        this.maxFpsWindow = [];
    }

    private drawPose(
        context: CanvasRenderingContext2D,
        width: number,
        height: number,
    ) {
        const pose = this.latestPose;
        if (!pose || pose.width !== width || pose.height !== height) {
            return;
        }

        // Central points of the shoulders and the pelvis, connected by a spine
        // line (the side shoulder→hip lines are intentionally not drawn).
        const shoulderCenter = midpoint(pose.keypoints[5], pose.keypoints[6]);
        const hipCenter = midpoint(pose.keypoints[11], pose.keypoints[12]);

        // MoveNet quirk: when the pelvis sits at the very bottom of the frame
        // and the knee/ankle points land above it, the legs are hallucinated
        // (e.g. the person is cut off by the frame). Hide them in that case.
        let hideLegs = false;
        if (hipCenter && hipCenter.y > height * PELVIS_BOTTOM_MARGIN_RATIO) {
            const visibleLegPoints = LEG_KEYPOINT_INDICES
                .map((index) => pose.keypoints[index])
                .filter((point) => point && point.score >= MIN_KEYPOINT_SCORE);

            hideLegs =
                visibleLegPoints.length > 0
                && visibleLegPoints.every((point) => point.y < hipCenter.y);
        }

        const isLegPoint = (index: number) => LEG_KEYPOINT_INDICES.includes(index);

        // Face: the eyes connect to their center, the center connects to the
        // nose.
        const eyeCenter = midpoint(pose.keypoints[1], pose.keypoints[2]);
        const nose = pose.keypoints[0];
        const leftEye = pose.keypoints[1];
        const rightEye = pose.keypoints[2];

        context.save();
        const radius = (KEYPOINT_RADIUS * width) / KEYPOINT_REFERENCE_WIDTH;
        const lineWidth = (SKELETON_LINE_WIDTH * width) / KEYPOINT_REFERENCE_WIDTH;

        // Bones first, so the joints sit on top of the line ends.
        context.strokeStyle = "#ffffff";
        context.lineWidth = lineWidth;
        context.lineCap = "round";
        for (const [from, to] of POSE_EDGES) {
            const a = pose.keypoints[from];
            const b = pose.keypoints[to];
            if (
                !a || !b
                || a.score < MIN_KEYPOINT_SCORE
                || b.score < MIN_KEYPOINT_SCORE
                || (hideLegs && (isLegPoint(from) || isLegPoint(to)))
            ) {
                continue;
            }

            context.beginPath();
            context.moveTo(a.x, a.y);
            context.lineTo(b.x, b.y);
            context.stroke();
        }

        // Spine: shoulders center ↔ pelvis center.
        if (shoulderCenter && hipCenter) {
            context.beginPath();
            context.moveTo(shoulderCenter.x, shoulderCenter.y);
            context.lineTo(hipCenter.x, hipCenter.y);
            context.stroke();
        }

        // Face: eyes ↔ eyes center ↔ nose.
        if (eyeCenter) {
            for (const eye of [leftEye, rightEye]) {
                if (!eye || eye.score < MIN_KEYPOINT_SCORE) {
                    continue;
                }

                context.beginPath();
                context.moveTo(eyeCenter.x, eyeCenter.y);
                context.lineTo(eye.x, eye.y);
                context.stroke();
            }
        }

        if (eyeCenter && nose && nose.score >= MIN_KEYPOINT_SCORE) {
            context.beginPath();
            context.moveTo(eyeCenter.x, eyeCenter.y);
            context.lineTo(nose.x, nose.y);
            context.stroke();
        }

        // Joints.
        context.fillStyle = "#ffffff";
        pose.keypoints.forEach((keypoint, index) => {
            if (keypoint.score < MIN_KEYPOINT_SCORE) {
                return;
            }

            if (hideLegs && isLegPoint(index)) {
                return;
            }

            context.beginPath();
            context.arc(keypoint.x, keypoint.y, radius, 0, Math.PI * 2);
            context.fill();
        });

        // Torso and face center points.
        for (const center of [shoulderCenter, hipCenter, eyeCenter]) {
            if (!center) {
                continue;
            }

            context.beginPath();
            context.arc(center.x, center.y, radius, 0, Math.PI * 2);
            context.fill();
        }

        context.restore();
    }

    render() {
        const {resolution, poseStats, mirror} = this.state;
        const {isStreaming, isStoppingCamera, onStop} = this.props;

        return (
            <div className="camera-preview">
                <canvas
                    ref={this.canvasRef}
                    className="camera"
                    aria-label="Camera preview"
                    style={{transform: mirror ? "scaleX(-1)" : undefined}}
                />
                <div className={`mt-2 is-flex is-flex-wrap-wrap is-justify-content-space-between is-align-items-center${isStreaming ? " status-card" : ""}`} style={{gap: "0.5rem", flexShrink: 0, position: "relative", zIndex: 1}}>
                    <div className="is-flex is-flex-wrap-wrap is-align-items-center" style={{gap: "0.5rem"}}>
                        {resolution && (
                            <Tag>{`${resolution.width}×${resolution.height}`}</Tag>
                        )}
                        {poseStats && (
                            <Tag color="info">
                                {`Recognition: ${poseStats.inferenceMs.toFixed(0)} ms`}
                            </Tag>
                        )}
                        {poseStats && (
                            <Tag color="info">
                                {`Max: ${poseStats.maxFps.toFixed(1)} FPS`}
                            </Tag>
                        )}
                        {poseStats?.provider && (
                            <Tag color={poseStats.provider === "GPU" ? "success" : "warning"}>
                                {`MoveNet: ${poseStats.provider}`}
                            </Tag>
                        )}
                    </div>
                    <div className="is-flex is-align-items-center" style={{gap: "0.5rem"}}>
                        {isStreaming && (
                            <>
                                <Button
                                    size="small"
                                    color={mirror ? "info" : undefined}
                                    aria-pressed={mirror}
                                    onClick={this.handleMirrorToggle}
                                >
                                    Mirror
                                </Button>
                                <Button
                                    size="small"
                                    color="danger"
                                    onClick={onStop}
                                    disabled={isStoppingCamera}
                                >
                                    {isStoppingCamera ? "Stopping…" : "Stop streaming"}
                                </Button>
                            </>
                        )}
                    </div>
                </div>
            </div>
        );
    }

    private handleMirrorToggle = () => {
        this.setState((state) => ({mirror: !state.mirror}));
    };
}
