package dev.tabscreen

import android.content.Context
import android.os.SystemClock
import android.util.AttributeSet
import android.view.MotionEvent
import android.view.SurfaceView
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.sin

/**
 * Renders the decoded stream and forwards S Pen (and optionally finger) input.
 * Coordinates are sent normalised to 0..1 of the view so the server can map
 * them onto the virtual monitor regardless of resolution.
 */
class StreamView @JvmOverloads constructor(context: Context, attrs: AttributeSet? = null) : SurfaceView(context, attrs) {

    interface InputSink {
        fun send(msg: ByteArray)
    }

    var sink: InputSink? = null
    var sendTouch = true
    /** Touchpad mode: everything (pen included) is a finger on a trackpad; no hover. */
    var touchpadMode = false
    /** Flip if the pen tilt feels mirrored on the desktop. */
    var invertTiltX = false
    var invertTiltY = false

    private val penBatch = Protocol.PenBatch(64)
    private val touchBatch = Protocol.TouchBatch(64)
    private var penInProximity = false

    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    private fun isStylus(ev: MotionEvent, idx: Int = ev.actionIndex): Boolean {
        val t = ev.getToolType(idx)
        return t == MotionEvent.TOOL_TYPE_STYLUS || t == MotionEvent.TOOL_TYPE_ERASER
    }

    override fun onTouchEvent(ev: MotionEvent): Boolean {
        // Ask the platform to deliver every sample immediately instead of batching to vsync.
        requestUnbufferedDispatch(ev)
        if (touchpadMode) return handleTouch(ev)
        return if (isStylus(ev)) handlePen(ev, touching = true) else handleTouch(ev)
    }

    override fun onGenericMotionEvent(ev: MotionEvent): Boolean {
        if (touchpadMode || !isStylus(ev)) return super.onGenericMotionEvent(ev)
        return when (ev.actionMasked) {
            MotionEvent.ACTION_HOVER_ENTER, MotionEvent.ACTION_HOVER_MOVE, MotionEvent.ACTION_HOVER_EXIT,
            MotionEvent.ACTION_BUTTON_PRESS, MotionEvent.ACTION_BUTTON_RELEASE -> handlePen(ev, touching = false)
            else -> super.onGenericMotionEvent(ev)
        }
    }

    private fun buttons(ev: MotionEvent): Int {
        var b = 0
        if (ev.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0) b = b or Protocol.BTN_PRIMARY
        if (ev.buttonState and MotionEvent.BUTTON_STYLUS_SECONDARY != 0) b = b or Protocol.BTN_SECONDARY
        return b
    }

    private fun handlePen(ev: MotionEvent, touching: Boolean): Boolean {
        val sink = sink ?: return true
        val tool = if (ev.getToolType(0) == MotionEvent.TOOL_TYPE_ERASER) Protocol.TOOL_ERASER else Protocol.TOOL_PEN
        val btn = buttons(ev)
        val w = width.toFloat().coerceAtLeast(1f)
        val h = height.toFloat().coerceAtLeast(1f)

        val action = when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> Protocol.PEN_DOWN
            MotionEvent.ACTION_MOVE -> Protocol.PEN_MOVE
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> Protocol.PEN_UP
            MotionEvent.ACTION_CANCEL -> Protocol.PEN_CANCEL
            MotionEvent.ACTION_HOVER_ENTER -> Protocol.PEN_HOVER_ENTER
            MotionEvent.ACTION_HOVER_MOVE -> Protocol.PEN_HOVER_MOVE
            MotionEvent.ACTION_HOVER_EXIT -> Protocol.PEN_HOVER_EXIT
            MotionEvent.ACTION_BUTTON_PRESS, MotionEvent.ACTION_BUTTON_RELEASE ->
                if (touching) Protocol.PEN_MOVE else Protocol.PEN_HOVER_MOVE
            else -> return true
        }
        penInProximity = action != Protocol.PEN_HOVER_EXIT && action != Protocol.PEN_CANCEL

        penBatch.reset()
        // Historical samples first (higher effective sample rate for strokes).
        if (action == Protocol.PEN_MOVE || action == Protocol.PEN_HOVER_MOVE) {
            for (i in 0 until ev.historySize) {
                val (tx, ty) = tilt(ev.getHistoricalAxisValue(MotionEvent.AXIS_TILT, 0, i), ev.getHistoricalAxisValue(MotionEvent.AXIS_ORIENTATION, 0, i))
                penBatch.add(
                    ev.getHistoricalEventTime(i) * 1_000_000L, action, tool, btn,
                    ev.getHistoricalX(0, i) / w, ev.getHistoricalY(0, i) / h,
                    ev.getHistoricalPressure(0, i), tx, ty,
                    ev.getHistoricalAxisValue(MotionEvent.AXIS_DISTANCE, 0, i),
                )
            }
        }
        val (tx, ty) = tilt(ev.getAxisValue(MotionEvent.AXIS_TILT), ev.getAxisValue(MotionEvent.AXIS_ORIENTATION))
        penBatch.add(
            ev.eventTime * 1_000_000L, action, tool, btn,
            ev.x / w, ev.y / h, ev.pressure, tx, ty, ev.getAxisValue(MotionEvent.AXIS_DISTANCE),
        )
        penBatch.build()?.let(sink::send)
        return true
    }

    /** Android tilt (angle from vertical) + orientation (compass-style) -> X/Y tilt in degrees. */
    private fun tilt(tiltRad: Float, orientationRad: Float): Pair<Float, Float> {
        val r = sin(tiltRad)
        val z = cos(tiltRad)
        var tx = Math.toDegrees(atan2(sin(orientationRad) * r, z).toDouble()).toFloat()
        var ty = Math.toDegrees(atan2(-cos(orientationRad) * r, z).toDouble()).toFloat()
        if (invertTiltX) tx = -tx
        if (invertTiltY) ty = -ty
        return tx to ty
    }

    private fun handleTouch(ev: MotionEvent): Boolean {
        val sink = sink ?: return true
        // Palm rejection: ignore fingers while the pen is near the screen.
        if (!sendTouch || penInProximity) return true
        val w = width.toFloat().coerceAtLeast(1f)
        val h = height.toFloat().coerceAtLeast(1f)
        touchBatch.reset()
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                val i = ev.actionIndex
                touchBatch.add(ev.eventTime * 1_000_000L, Protocol.TOUCH_DOWN, ev.getPointerId(i), ev.getX(i) / w, ev.getY(i) / h, ev.getPressure(i), ev.getTouchMajor(i) / w)
            }
            MotionEvent.ACTION_MOVE -> {
                for (hist in 0 until ev.historySize) {
                    for (i in 0 until ev.pointerCount) {
                        touchBatch.add(ev.getHistoricalEventTime(hist) * 1_000_000L, Protocol.TOUCH_MOVE, ev.getPointerId(i), ev.getHistoricalX(i, hist) / w, ev.getHistoricalY(i, hist) / h, ev.getHistoricalPressure(i, hist), ev.getHistoricalTouchMajor(i, hist) / w)
                    }
                }
                for (i in 0 until ev.pointerCount) {
                    touchBatch.add(ev.eventTime * 1_000_000L, Protocol.TOUCH_MOVE, ev.getPointerId(i), ev.getX(i) / w, ev.getY(i) / h, ev.getPressure(i), ev.getTouchMajor(i) / w)
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val i = ev.actionIndex
                touchBatch.add(ev.eventTime * 1_000_000L, Protocol.TOUCH_UP, ev.getPointerId(i), ev.getX(i) / w, ev.getY(i) / h, 0f, 0f)
            }
            MotionEvent.ACTION_CANCEL -> {
                for (i in 0 until ev.pointerCount) {
                    touchBatch.add(ev.eventTime * 1_000_000L, Protocol.TOUCH_CANCEL, ev.getPointerId(i), ev.getX(i) / w, ev.getY(i) / h, 0f, 0f)
                }
            }
            else -> return true
        }
        touchBatch.build()?.let(sink::send)
        return true
    }

    companion object {
        fun nowNs(): Long = SystemClock.uptimeMillis() * 1_000_000L
    }
}
