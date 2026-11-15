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

        context.save();
        context.fillStyle = "#00e676";
        const radius = (KEYPOINT_RADIUS * width) / KEYPOINT_REFERENCE_WIDTH;
        for (const keypoint of pose.keypoints) {
            if (keypoint.score < MIN_KEYPOINT_SCORE) {
                continue;
            }

            context.beginPath();
            context.arc(keypoint.x, keypoint.y, radius, 0, Math.PI * 2);
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
                <div className="mt-2 mx-3 mb-3 is-flex is-justify-content-space-between is-align-items-center" style={{gap: "0.5rem", flexShrink: 0}}>
                    <div className="is-flex is-align-items-center" style={{gap: "0.5rem"}}>
                        {resolution && (
                            <Tag>{`${resolution.width}×${resolution.height}`}</Tag>
                        )}
                        {poseStats && (
                            <Tag color="info">
                                {`Recognition: ${poseStats.inferenceMs.toFixed(0)} ms · max ${poseStats.maxFps.toFixed(1)} FPS`}
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
                                    color={mirror ? "primary" : undefined}
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
