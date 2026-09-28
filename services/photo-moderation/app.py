import asyncio
import os
import secrets
from contextlib import asynccontextmanager

import cv2
import numpy as np
from fastapi import FastAPI, Header, HTTPException, Request, status
from opennsfw_onnx import NSFWClassifier

MAX_BODY_BYTES = 500_000
MAX_IMAGE_PIXELS = 16_777_216

token = os.environ.get("MODERATION_TOKEN", "")
if len(token.encode("utf-8")) < 32:
    raise RuntimeError("MODERATION_TOKEN must contain at least 32 bytes")

face_detector = cv2.CascadeClassifier(
    cv2.data.haarcascades + "haarcascade_frontalface_default.xml"
)
if face_detector.empty():
    raise RuntimeError("OpenCV face cascade could not be loaded")

classifier = NSFWClassifier(
    providers=["CPUExecutionProvider"],
    intra_op_num_threads=1,
    jpeg_reencode=False,
)
analysis_lock = asyncio.Lock()


@asynccontextmanager
async def lifespan(_: FastAPI):
    await asyncio.to_thread(classifier.warmup)
    yield


app = FastAPI(
    title="Histae internal photo moderation",
    docs_url=None,
    redoc_url=None,
    openapi_url=None,
    lifespan=lifespan,
)


@app.get("/health")
async def health() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/v1/analyze")
async def analyze(
    request: Request,
    authorization: str | None = Header(default=None),
) -> dict[str, int | float]:
    expected = f"Bearer {token}"
    if authorization is None or not secrets.compare_digest(authorization, expected):
        raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="unauthorized")
    if request.headers.get("content-type", "").split(";", 1)[0].strip().lower() != "image/webp":
        raise HTTPException(status_code=status.HTTP_415_UNSUPPORTED_MEDIA_TYPE, detail="webp required")

    body = bytearray()
    async for chunk in request.stream():
        body.extend(chunk)
        if len(body) > MAX_BODY_BYTES:
            raise HTTPException(status_code=status.HTTP_413_REQUEST_ENTITY_TOO_LARGE, detail="image too large")
    if not body:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail="empty image")

    async with analysis_lock:
        return await asyncio.to_thread(analyze_image, bytes(body))


def analyze_image(body: bytes) -> dict[str, int | float]:
    encoded = np.frombuffer(body, dtype=np.uint8)
    image = cv2.imdecode(encoded, cv2.IMREAD_COLOR)
    if image is None:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail="invalid image")
    height, width = image.shape[:2]
    if height <= 0 or width <= 0 or height * width > MAX_IMAGE_PIXELS:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail="invalid dimensions")

    grayscale = cv2.cvtColor(image, cv2.COLOR_BGR2GRAY)
    normalized = cv2.equalizeHist(grayscale)
    minimum = max(30, min(width, height) // 10)
    faces = face_detector.detectMultiScale(
        normalized,
        scaleFactor=1.1,
        minNeighbors=5,
        minSize=(minimum, minimum),
    )
    sharpness = float(cv2.Laplacian(grayscale, cv2.CV_64F).var())
    nsfw_score = float(classifier.classify(body).nsfw)
    return {
        "face_count": int(len(faces)),
        "sharpness_score": round(sharpness, 4),
        "nsfw_score": round(nsfw_score, 6),
    }
