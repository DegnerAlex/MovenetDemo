import {
    Box,
    Button,
    Heading,
    Loader,
    Notification,
    Tag,
} from "react-bulma-components";

export type CameraDescriptor = {
    index: string;
    name: string;
};

// One resolution entry from the backend: the FPS comes with the desktop
// backend (read from the camera's frame intervals); Android leaves it out.
export type ResolutionInfo = {
    resolution: string;
    maxFps?: number | null;
};

type CameraSettingsProps = {
    cameras: CameraDescriptor[];
    selectedCamera: string;
    resolutions: ResolutionInfo[];
    selectedResolution: string;
    isLoadingCameras: boolean;
    isLoadingResolutions: boolean;
    isStartingCamera: boolean;
    isStoppingCamera: boolean;
    isStreaming: boolean;
    cameraError: string;
    onCameraSelect: (cameraIndex: string) => void;
    onResolutionSelect: (resolution: string) => void;
    onRefresh: () => void;
    onStart: () => void;
    onStop: () => void;
};

/**
 * Presentational panel: camera picker, resolution picker and the
 * start/stop streaming controls. All state lives in the parent.
 */
export function CameraSettings(props: CameraSettingsProps) {
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
        onCameraSelect,
        onResolutionSelect,
        onRefresh,
        onStart,
        onStop,
    } = props;

    return (
        <Box className="settings-card camera-settings">
            <div className="is-flex is-justify-content-space-between is-align-items-center mb-3">
                <Heading size={5} className="mb-0">Choose the camera</Heading>
                <Button
                    size="small"
                    onClick={onRefresh}
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
                                size="small"
                                color={isSelected ? "info" : undefined}
                                aria-pressed={isSelected}
                                onClick={() => onCameraSelect(camera.index)}
                                disabled={isStreaming || isStartingCamera}
                            >
                                {camera.name || `Camera ${camera.index}`}
                            </Button>
                        );
                    })}
                </Button.Group>
            )}

            {isLoadingResolutions ? (
                <div className="is-flex is-align-items-center mt-2" style={{gap: "0.75rem"}}>
                    <Loader/>
                    <span>Loading resolutions…</span>
                </div>
            ) : resolutions.length === 0 ? (
                <div className="mt-2">
                    <Tag>Auto (camera decides)</Tag>
                </div>
            ) : (
                <Button.Group
                    aria-label="Resolution selection"
                    className="is-flex-wrap-wrap mt-2"
                >
                    {resolutions.map((value) => {
                        const isSelected = value.resolution === selectedResolution;
                        const label = value.maxFps
                            ? `${value.resolution} · ${value.maxFps} FPS`
                            : value.resolution;

                        return (
                            <Button
                                key={value.resolution}
                                size="small"
                                color={isSelected ? "info" : undefined}
                                aria-pressed={isSelected}
                                onClick={() => onResolutionSelect(value.resolution)}
                                disabled={isStreaming || isStartingCamera}
                            >
                                {label}
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

            <div className="is-flex is-align-items-center mt-3"
                 style={{gap: "0.75rem", justifyContent: isStreaming ? "flex-end" : "center"}}>
                {isStreaming ? (
                    <>
                        <Tag color="success">Streaming active</Tag>
                        <Button
                            color="danger"
                            onClick={onStop}
                            disabled={isStoppingCamera}
                        >
                            {isStoppingCamera ? "Stopping…" : "Stop streaming"}
                        </Button>
                    </>
                ) : (
                    <Button
                        color="success"
                        onClick={onStart}
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
    );
}
