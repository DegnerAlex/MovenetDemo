import {Component, createRef} from "react";
import {Channel, invoke} from "@tauri-apps/api/core";
import {
    Box,
    Button,
    Container,
    Heading,
    Loader,
    Notification,
    Section,
    Tag,
} from "react-bulma-components";
import "./App.css";
import {error, info} from "@tauri-apps/plugin-log";

type AppProps = Record<string, never>;

type CameraDescriptor = {
    index: string;
    name: string;
};

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

const MIN_KEYPOINT_SCORE = 0.3;

// Skeleton dot radius at the reference resolution; scales proportionally
// to the frame width (the canvas stretches to the container width).
const KEYPOINT_RADIUS = 10;
const KEYPOINT_REFERENCE_WIDTH = 1024;

// Sliding-average window for recognition time and FPS on the badge.
const SMOOTHING_WINDOW = 10;
const FPS_SMOOTHING_WINDOW = 20;

type PoseStats = {
    inferenceMs: number;
    maxFps: number;
    provider: string;
};

type Resolution = {
    width: number;
    height: number;
};

type AppState = {
    cameras: CameraDescriptor[];
    selectedCamera: string;
    resolutions: string[];
    selectedResolution: string;
    isLoadingCameras: boolean;
    isLoadingResolutions: boolean;
    isStartingCamera: boolean;
    isStoppingCamera: boolean;
    isStreaming: boolean;
    cameraError: string;
    poseStats: PoseStats | null;
    resolution: Resolution | null;
    mirror: boolean;
};

// Frame header: width, height and JSON tail length with the recognized pose (u32 big-endian).
const FRAME_HEADER_BYTES = 12;

class App extends Component<AppProps, AppState> {
    state: AppState = {
        cameras: [],
        selectedCamera: "",
        resolutions: [],
        selectedResolution: "",
        isLoadingCameras: true,
        isLoadingResolutions: false,
        isStartingCamera: false,
        isStoppingCamera: false,
        isStreaming: false,
        cameraError: "",
        poseStats: null,
        resolution: null,
        mirror: true,
    };

    private readonly canvasRef = createRef<HTMLCanvasElement>();
    private imageData: ImageData | null = null;
    private pendingFrame: ArrayBuffer | null = null;
    private frameRequest: number | null = null;
    private frameChannel: Channel<ArrayBuffer> | null = null;
    private streamStarting = false;
    private streamStopping = false;
    private latestPose: PoseEstimate | null = null;
    private inferenceMsWindow: number[] = [];
    private maxFpsWindow: number[] = [];

    componentDidMount() {
        void this.loadCameras();
    }

    private loadCameras = async () => {
        this.setState({isLoadingCameras: true, cameraError: ""});

        try {
            const cameras = await invoke<CameraDescriptor[]>("list_cameras");
            const selectedCamera = cameras.some((camera) => camera.index === this.state.selectedCamera)
                ? this.state.selectedCamera
                : cameras[0]?.index ?? "";

            this.setState({
                cameras,
                selectedCamera,
                isLoadingCameras: false,
            });

            if (selectedCamera) {
                void this.loadResolutions(selectedCamera);
            }
        } catch (err) {
            await error(`list_cameras: ${err}`);
            this.setState({
                isLoadingCameras: false,
                cameraError: String(err),
            });
        }
    };

    private loadResolutions = async (cameraIndex: string) => {
        this.setState({isLoadingResolutions: true, resolutions: [], selectedResolution: ""});

        try {
            const resolutions = await invoke<string[]>("list_resolutions", {cameraIndex});
            await info(`list_resolutions: ${resolutions.length} for camera ${cameraIndex}`);

            this.setState({
                resolutions,
                selectedResolution: resolutions[0] ?? "",
                isLoadingResolutions: false,
            });
        } catch (err) {
            await error(`list_resolutions: ${err}`);
            this.setState({
                isLoadingResolutions: false,
                cameraError: String(err),
            });
        }
    };

    private startSelectedCamera = () => {
        if (
            this.streamStarting
            || this.streamStopping
            || this.state.isStreaming
            || !this.state.selectedCamera
        ) {
            return;
        }

        void this.startStreaming(this.state.selectedCamera);
    };

    private startStreaming = async (cameraIndex: string) => {
        const onFrame = new Channel<ArrayBuffer>();
        onFrame.onmessage = (frame) => {
            // Keep only the latest frame; draw it on the next animation frame.
            this.pendingFrame = frame;
            if (this.frameRequest === null) {
                this.frameRequest = requestAnimationFrame(this.drawPendingFrame);
            }
        };

        this.frameChannel = onFrame;
        this.streamStarting = true;
        this.latestPose = null;
        this.resetPoseWindows();
        this.setState({isStartingCamera: true, cameraError: ""});

        try {
            await invoke("start_rust_camera", {
                cameraIndex,
                resolution: this.state.selectedResolution || null,
                onFrame,
            });
            await info(`start_rust_camera: started (${cameraIndex})`);
            this.setState({isStartingCamera: false, isStreaming: true});
        } catch (err) {
            await error(`start_rust_camera: ${err}`);
            this.detachFrameChannel();
            this.setState({
                isStartingCamera: false,
                cameraError: String(err),
            });
        } finally {
            this.streamStarting = false;
        }
    };

    private stopSelectedCamera = () => {
        if (this.streamStopping || !this.state.isStreaming) {
            return;
        }

        void this.stopStreaming();
    };

    private stopStreaming = async () => {
        this.streamStopping = true;
        this.setState({isStoppingCamera: true, cameraError: ""});

        try {
            await invoke("stop_rust_camera");
            await info("stop_rust_camera: stopped");
            this.detachFrameChannel();
            this.setState({isStoppingCamera: false, isStreaming: false});
        } catch (err) {
            await error(`stop_rust_camera: ${err}`);
            this.setState({
                isStoppingCamera: false,
                cameraError: String(err),
            });
        } finally {
            this.streamStopping = false;
        }
    };

    private detachFrameChannel() {
        if (this.frameChannel) {
            this.frameChannel.onmessage = () => {
            };
            this.frameChannel = null;
        }

        if (this.frameRequest !== null) {
            cancelAnimationFrame(this.frameRequest);
            this.frameRequest = null;
        }

        this.pendingFrame = null;
        this.imageData = null;
        this.latestPose = null;
        this.resetPoseWindows();
        this.setState({poseStats: null, resolution: null});

        const canvas = this.canvasRef.current;
        const context = canvas?.getContext("2d");
        if (canvas && context) {
            context.clearRect(0, 0, canvas.width, canvas.height);
        }
    }

    componentWillUnmount() {
        void invoke("stop_rust_camera").catch((err) => error(`stop_rust_camera: ${err}`));
        this.detachFrameChannel();
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

    handleCameraSelect = (cameraIndex: string) => {
        if (!this.state.isStreaming) {
            this.setState({selectedCamera: cameraIndex, cameraError: ""});
            void this.loadResolutions(cameraIndex);
        }
    };

    handleResolutionSelect = (value: string) => {
        if (!this.state.isStreaming) {
            this.setState({selectedResolution: value});
        }
    };

    handleMirrorToggle = () => {
        this.setState((state) => ({mirror: !state.mirror}));
    };

    handleCameraRefresh = () => {
        void this.loadCameras();
    };

    render() {
        const {
            cameras,
            selectedCamera,
            resolutions,
            selectedResolution,
            isLoadingCameras,
            isLoadingResolutions,
            isStartingCamera,
            isStoppingCamera,
            isStreaming,
            cameraError,
            poseStats,
            resolution,
            mirror,
        } = this.state;

        return (
            <Section>
                <Container>
                    <Box>
                        <canvas
                            ref={this.canvasRef}
                            className="camera"
                            aria-label="Camera preview"
                            style={{transform: mirror ? "scaleX(-1)" : undefined}}
                        />
                        <div className="mt-2 is-flex is-justify-content-space-between is-align-items-center" style={{gap: "0.5rem"}}>
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
                            <Button
                                size="small"
                                color={mirror ? "primary" : undefined}
                                aria-pressed={mirror}
                                onClick={this.handleMirrorToggle}
                            >
                                Mirror
                            </Button>
                        </div>
                    </Box>

                    <Box>
                        <div className="is-flex is-justify-content-space-between is-align-items-center mb-5">
                            <Heading size={5} className="mb-0">Webcam</Heading>
                            <Button
                                onClick={this.handleCameraRefresh}
                                disabled={isLoadingCameras || isStreaming || isStoppingCamera}
                            >
                                Refresh
                            </Button>
                        </div>

                        {isLoadingCameras ? (
                            <div className="is-flex is-align-items-center" style={{gap: "0.75rem"}}>
                                <Loader/>
                                <span>Searching for cameras…</span>
                            </div>
                        ) : cameras.length === 0 ? (
                            <p>No cameras found.</p>
                        ) : (
                            <Button.Group aria-label="Camera selection">
                                {cameras.map((camera) => {
                                    const isSelected = camera.index === selectedCamera;

                                    return (
                                        <Button
                                            key={camera.index}
                                            color={isSelected ? "primary" : undefined}
                                            aria-pressed={isSelected}
                                            onClick={() => this.handleCameraSelect(camera.index)}
                                            disabled={isStreaming || isStartingCamera}
                                        >
                                            {camera.name || `Camera ${camera.index}`}
                                        </Button>
                                    );
                                })}
                            </Button.Group>
                        )}

                        {isLoadingResolutions ? (
                            <div className="is-flex is-align-items-center mt-3" style={{gap: "0.75rem"}}>
                                <Loader/>
                                <span>Loading resolutions…</span>
                            </div>
                        ) : resolutions.length === 0 ? (
                            <div className="mt-3">
                                <Tag>Auto (camera decides)</Tag>
                            </div>
                        ) : (
                            <Button.Group
                                aria-label="Resolution selection"
                                className="is-flex-wrap-wrap mt-3"
                            >
                                    {resolutions.map((value) => {
                                        const isSelected = value === selectedResolution;

                                        return (
                                            <Button
                                                key={value}
                                                color={isSelected ? "primary" : undefined}
                                                aria-pressed={isSelected}
                                                onClick={() => this.handleResolutionSelect(value)}
                                                disabled={isStreaming || isStartingCamera}
                                            >
                                                {value}
                                            </Button>
                                        );
                                    })}
                            </Button.Group>
                        )}

                        {cameraError && (
                            <Notification color="danger" className="mt-4">
                                {cameraError}
                            </Notification>
                        )}

                        <div className="is-flex is-justify-content-flex-end is-align-items-center mt-5"
                             style={{gap: "0.75rem"}}>
                            {isStreaming ? (
                                <>
                                    <Tag color="success">Streaming active</Tag>
                                    <Button
                                        color="danger"
                                        onClick={this.stopSelectedCamera}
                                        disabled={isStoppingCamera}
                                    >
                                        {isStoppingCamera ? "Stopping…" : "Stop streaming"}
                                    </Button>
                                </>
                            ) : (
                                <Button
                                    color="primary"
                                    onClick={this.startSelectedCamera}
                                    disabled={
                                        isLoadingCameras
                                        || isStartingCamera
                                        || isStoppingCamera
                                        || !selectedCamera
                                    }
                                >
                                    {isStartingCamera ? "Starting…" : "Start streaming"}
                                </Button>
                            )}
                        </div>
                    </Box>
                </Container>
            </Section>
        );
    }
}

export default App;
