import {Component, createRef} from "react";

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

export type PoseKeypoint = {
    name: string;
    /** Normalized to the frame (0..1); scaled to the canvas for drawing. */
    x: number;
    y: number;
    score: number;
};

export type PoseEstimate = {
    width: number;
    height: number;
    keypoints: PoseKeypoint[];
    inferenceMs: number;
    maxFps: number;
    provider: string;
};

export type PoseStats = {
    inferenceMs: number;
    maxFps: number;
    provider: string;
};

// Midpoint of two keypoints, or `null` when either point is missing or too
// uncertain.
function midpoint(a?: PoseKeypoint, b?: PoseKeypoint): {x: number; y: number} | null {
    if (!a || !b || a.score < MIN_KEYPOINT_SCORE || b.score < MIN_KEYPOINT_SCORE) {
        return null;
    }

    return {x: (a.x + b.x) / 2, y: (a.y + b.y) / 2};
}

type PoseSkeletonProps = {
    pose: PoseEstimate | null;
    mirror: boolean;
};

/**
 * Transparent overlay canvas that draws the skeleton (bones, spine, face
 * star, joints) for the given pose. Stacked over the video canvas with
 * identical geometry, so the drawings align pixel-perfectly.
 */
export class PoseSkeleton extends Component<PoseSkeletonProps> {
    private readonly canvasRef = createRef<HTMLCanvasElement>();

    componentDidMount() {
        this.redraw();
    }

    componentDidUpdate(prevProps: PoseSkeletonProps) {
        if (prevProps.pose !== this.props.pose || prevProps.mirror !== this.props.mirror) {
            this.redraw();
        }
    }

    private redraw() {
        const canvas = this.canvasRef.current;
        if (!canvas) {
            return;
        }

        const pose = this.props.pose;
        if (!pose || pose.width === 0 || pose.height === 0) {
            const context = canvas.getContext("2d");
            context?.clearRect(0, 0, canvas.width, canvas.height);
            return;
        }

        if (canvas.width !== pose.width || canvas.height !== pose.height) {
            canvas.width = pose.width;
            canvas.height = pose.height;
        }

        const context = canvas.getContext("2d");
        if (!context) {
            return;
        }

        context.clearRect(0, 0, canvas.width, canvas.height);
        this.drawSkeleton(context, pose);
    }

    private drawSkeleton(context: CanvasRenderingContext2D, pose: PoseEstimate) {
        const width = pose.width;
        const height = pose.height;

        // Keypoints arrive normalized to the frame (0..1): scale them to the
        // canvas once, so every drawing below works in canvas pixels.
        const points: PoseKeypoint[] = pose.keypoints.map((keypoint) => ({
            ...keypoint,
            x: keypoint.x * width,
            y: keypoint.y * height,
        }));

        // Central points of the shoulders and the pelvis, connected by a spine
        // line (the side shoulder→hip lines are intentionally not drawn).
        const shoulderCenter = midpoint(points[5], points[6]);
        const hipCenter = midpoint(points[11], points[12]);

        // MoveNet quirk: when the pelvis sits at the very bottom of the frame
        // and the knee/ankle points land above it, the legs are hallucinated
        // (e.g. the person is cut off by the frame). Hide them in that case.
        let hideLegs = false;
        if (hipCenter && hipCenter.y > height * PELVIS_BOTTOM_MARGIN_RATIO) {
            const visibleLegPoints = LEG_KEYPOINT_INDICES
                .map((index) => points[index])
                .filter((point) => point && point.score >= MIN_KEYPOINT_SCORE);

            hideLegs =
                visibleLegPoints.length > 0
                && visibleLegPoints.every((point) => point.y < hipCenter.y);
        }

        const isLegPoint = (index: number) => LEG_KEYPOINT_INDICES.includes(index);

        // Face: the eyes connect to their center, the center connects to the
        // nose.
        const eyeCenter = midpoint(points[1], points[2]);
        const nose = points[0];
        const leftEye = points[1];
        const rightEye = points[2];

        context.save();
        const radius = (KEYPOINT_RADIUS * width) / KEYPOINT_REFERENCE_WIDTH;
        const lineWidth = (SKELETON_LINE_WIDTH * width) / KEYPOINT_REFERENCE_WIDTH;

        // Bones first, so the joints sit on top of the line ends.
        context.strokeStyle = "#ffffff";
        context.lineWidth = lineWidth;
        context.lineCap = "round";
        for (const [from, to] of POSE_EDGES) {
            const a = points[from];
            const b = points[to];
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
        points.forEach((keypoint, index) => {
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
        const {mirror} = this.props;

        return (
            <canvas
                ref={this.canvasRef}
                className="pose-skeleton"
                aria-hidden="true"
                style={{transform: mirror ? "scaleX(-1)" : undefined}}
            />
        );
    }
}
