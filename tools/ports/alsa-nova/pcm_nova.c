/* ALSA PCM plugin "nova": playback into Nova's mixer through
 * /dev/nova-audio (kernel side: src/linux/audio.rs).
 *
 * alsa-lib's ioplug does the ALSA bookkeeping (hardware parameters,
 * application pointer, poll); this file says where the hardware pointer
 * is and where the frames go.
 *
 * The device takes one format: interleaved stereo S16_LE at 48 kHz, the
 * mixer's own. The plugin accepts what programs actually send -- Firefox
 * sends 32-bit floats at 44.1 or 48 kHz -- and converts here, in user
 * space where floating point is allowed: float or s16, mono or stereo,
 * any rate, resampled by 4-point cubic (Catmull-Rom) interpolation.
 * alsa-lib's own "plug" layer did the same, but its rate converter and
 * ioplug's integer constraints refused one latency in two ("Invalid
 * argument" at 11.6 ms, accepted at 10 and 20), and Firefox's first
 * probe asks for 2 ms: it concluded there was no sound card.
 *
 * The hardware pointer: every frame converted enters the device's
 * queue, the mixer drains it. So the application frames played are the
 * frames written minus what is still queued (in the device, and in the
 * plugin when the device was full), counted back at the application's
 * rate -- modulo the buffer size, as ioplug wants, and never backwards.
 *
 * Waking: the device is told at prepare time how deep its queue may be
 * for the application to have a period to write (NOVA_THRESHOLD), and
 * reports POLLOUT only then. Without it the descriptor was always
 * writable and cubeb's poll loop spun on it for as long as sound played.
 *
 * Delay: what the device queues plus what the card's ring still holds
 * (NOVA_DELAY), read from the card's position. It moves sample by
 * sample, and it is what cubeb turns into the stream position a video
 * is timed on. Older kernels without it get the queue plus an average.
 */
#include <alsa/asoundlib.h>
#include <alsa/pcm_external.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define NOVA_PENDING 0x4E41
#define NOVA_FLUSH 0x4E42
#define NOVA_THRESHOLD 0x4E43
#define NOVA_DELAY 0x4E44
#define DEVICE_RATE 48000
/* Application buffer at most: 16384 frames, 341 ms at 48 kHz -- under the
 * half second the device queues, so what ALSA lets through always fits. */
#define MAX_BUFFER_FRAMES 16384
/* What the AC'97 ring holds ahead of the loudspeaker, on average. */
#define RING_LATENCY 2048

typedef struct {
    snd_pcm_ioplug_t io;
    int fd;
    /* Application frames written since prepare. */
    uint64_t written;
    /* Last hardware position returned: it never goes back. */
    uint64_t played;
    /* Resampler: position of the next output frame between hist[1] and
     * hist[2] of the four-frame history, in input frames. */
    double phase, step;
    float hist[4][2];
    /* Converted samples the device had no room for yet. */
    int16_t *out;
    size_t out_len, out_cap;
} nova_t;

static snd_pcm_sframes_t device_pending(nova_t *nova) {
    long n = ioctl(nova->fd, NOVA_PENDING, 0);
    return n < 0 ? 0 : n;
}

/* Device frames not played yet: in the device and in our own queue. */
static uint64_t queued(nova_t *nova) {
    return (uint64_t)device_pending(nova) + nova->out_len / 2;
}

static void flush_out(nova_t *nova) {
    while (nova->out_len > 0) {
        ssize_t n = write(nova->fd, nova->out, nova->out_len * 2);
        if (n <= 0)
            return;
        size_t samples = n / 2;
        memmove(nova->out, nova->out + samples, (nova->out_len - samples) * 2);
        nova->out_len -= samples;
    }
}

static void reset(nova_t *nova) {
    ioctl(nova->fd, NOVA_FLUSH, 0);
    nova->written = nova->played = 0;
    nova->out_len = 0;
    nova->phase = 0;
    memset(nova->hist, 0, sizeof(nova->hist));
    nova->step = (double)nova->io.rate / DEVICE_RATE;
}

static int nova_start(snd_pcm_ioplug_t *io) {
    (void)io;
    return 0;
}

static int nova_stop(snd_pcm_ioplug_t *io) {
    nova_t *nova = io->private_data;
    ioctl(nova->fd, NOVA_FLUSH, 0);
    nova->out_len = 0;
    return 0;
}

static int nova_prepare(snd_pcm_ioplug_t *io) {
    nova_t *nova = io->private_data;
    reset(nova);
    /* Writable once a period is free in the application buffer: queued
     * frames down to buffer - period, in device frames. */
    snd_pcm_uframes_t keep = io->buffer_size > io->period_size ? io->buffer_size - io->period_size : 0;
    unsigned long frames = (unsigned long)((uint64_t)keep * DEVICE_RATE / io->rate);
    ioctl(nova->fd, NOVA_THRESHOLD, frames);
    return 0;
}

static snd_pcm_sframes_t nova_pointer(snd_pcm_ioplug_t *io) {
    nova_t *nova = io->private_data;
    flush_out(nova);
    /* Queued device frames, back at the application's rate. */
    uint64_t back = (queued(nova) * io->rate + DEVICE_RATE - 1) / DEVICE_RATE;
    uint64_t played = nova->written > back ? nova->written - back : 0;
    if (played < nova->played)
        played = nova->played;
    /* A whole buffer played since the last look: the queue ran dry. Told
     * as such (an underrun), since modulo the buffer size the pointer
     * would read as not having moved at all -- and ALSA would wait for it
     * forever. */
    if (played - nova->played >= io->buffer_size && played > 0) {
        nova->played = played;
        return -EPIPE;
    }
    nova->played = played;
    return played % io->buffer_size;
}

static float sample_at(const snd_pcm_channel_area_t *areas, snd_pcm_format_t format, unsigned channels,
                       snd_pcm_uframes_t frame, unsigned channel) {
    const char *base = (const char *)areas->addr + (areas->first + areas->step * frame) / 8;
    unsigned c = channel < channels ? channel : channels - 1;
    if (format == SND_PCM_FORMAT_FLOAT_LE)
        return ((const float *)base)[c];
    return ((const int16_t *)base)[c] / 32768.0f;
}

static int16_t to_s16(float v) {
    if (v > 1.0f)
        v = 1.0f;
    if (v < -1.0f)
        v = -1.0f;
    return (int16_t)(v * 32767.0f);
}

static int reserve(nova_t *nova, size_t more) {
    if (nova->out_len + more <= nova->out_cap)
        return 0;
    size_t cap = (nova->out_len + more) * 2;
    int16_t *p = realloc(nova->out, cap * 2);
    if (!p)
        return -ENOMEM;
    nova->out = p;
    nova->out_cap = cap;
    return 0;
}

static snd_pcm_sframes_t nova_transfer(snd_pcm_ioplug_t *io, const snd_pcm_channel_area_t *areas,
                                       snd_pcm_uframes_t offset, snd_pcm_uframes_t size) {
    nova_t *nova = io->private_data;
    /* At most this many output frames per input frame, plus margin. */
    if (reserve(nova, ((size_t)(size / nova->step) + 4) * 2) < 0)
        return -ENOMEM;
    for (snd_pcm_uframes_t i = 0; i < size; i++) {
        /* Shift the history and take the next input frame. */
        for (unsigned k = 0; k < 3; k++) {
            nova->hist[k][0] = nova->hist[k + 1][0];
            nova->hist[k][1] = nova->hist[k + 1][1];
        }
        for (unsigned c = 0; c < 2; c++)
            nova->hist[3][c] = sample_at(areas, io->format, io->channels, offset + i, c);
        /* Output frames falling between hist[1] and hist[2]. */
        while (nova->phase < 1.0) {
            float t = (float)nova->phase;
            for (unsigned c = 0; c < 2; c++) {
                float p0 = nova->hist[0][c], p1 = nova->hist[1][c], p2 = nova->hist[2][c], p3 = nova->hist[3][c];
                float v = p1 + 0.5f * t * (p2 - p0 + t * (2 * p0 - 5 * p1 + 4 * p2 - p3 + t * (3 * (p1 - p2) + p3 - p0)));
                nova->out[nova->out_len++] = to_s16(v);
            }
            nova->phase += nova->step;
        }
        nova->phase -= 1.0;
    }
    nova->written += size;
    flush_out(nova);
    return size;
}

static int nova_delay(snd_pcm_ioplug_t *io, snd_pcm_sframes_t *delayp) {
    nova_t *nova = io->private_data;
    long device = ioctl(nova->fd, NOVA_DELAY, 0);
    uint64_t frames = device >= 0 ? (uint64_t)device + nova->out_len / 2 : queued(nova) + RING_LATENCY;
    *delayp = (snd_pcm_sframes_t)(frames * io->rate / DEVICE_RATE);
    return 0;
}

static int nova_drain(snd_pcm_ioplug_t *io) {
    nova_t *nova = io->private_data;
    while (queued(nova) > 0) {
        flush_out(nova);
        usleep(10000);
    }
    return 0;
}

static int nova_close(snd_pcm_ioplug_t *io) {
    nova_t *nova = io->private_data;
    close(nova->fd);
    free(nova->out);
    free(nova);
    return 0;
}

static const snd_pcm_ioplug_callback_t callbacks = {
    .start = nova_start,
    .stop = nova_stop,
    .prepare = nova_prepare,
    .pointer = nova_pointer,
    .transfer = nova_transfer,
    .delay = nova_delay,
    .drain = nova_drain,
    .close = nova_close,
};

static int constrain(snd_pcm_ioplug_t *io) {
    static const unsigned int access[] = {SND_PCM_ACCESS_RW_INTERLEAVED, SND_PCM_ACCESS_MMAP_INTERLEAVED};
    static const unsigned int format[] = {SND_PCM_FORMAT_S16_LE, SND_PCM_FORMAT_FLOAT_LE};
    int err;
    if ((err = snd_pcm_ioplug_set_param_list(io, SND_PCM_IOPLUG_HW_ACCESS, 2, access)) < 0 ||
        (err = snd_pcm_ioplug_set_param_list(io, SND_PCM_IOPLUG_HW_FORMAT, 2, format)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_CHANNELS, 1, 2)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_RATE, 8000, 192000)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_PERIOD_BYTES, 64, MAX_BUFFER_FRAMES * 8 / 2)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_PERIODS, 2, 1024)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_BUFFER_BYTES, 128, MAX_BUFFER_FRAMES * 8)) < 0)
        return err;
    return 0;
}

SND_PCM_PLUGIN_DEFINE_FUNC(nova) {
    snd_config_iterator_t i, next;
    (void)root;
    snd_config_for_each(i, next, conf) {
        snd_config_t *n = snd_config_iterator_entry(i);
        const char *id;
        if (snd_config_get_id(n, &id) < 0)
            continue;
        if (strcmp(id, "comment") == 0 || strcmp(id, "type") == 0 || strcmp(id, "hint") == 0)
            continue;
        SNDERR("Unknown field %s", id);
        return -EINVAL;
    }
    if (stream != SND_PCM_STREAM_PLAYBACK)
        return -ENOTSUP;

    nova_t *nova = calloc(1, sizeof(*nova));
    if (!nova)
        return -ENOMEM;
    /* NOVA_AUDIO_DEVICE: another file in place of the device, for
     * testing the plugin on a Linux host (/dev/null). */
    const char *path = getenv("NOVA_AUDIO_DEVICE");
    nova->fd = open(path ? path : "/dev/nova-audio", O_WRONLY | O_NONBLOCK | O_CLOEXEC);
    if (nova->fd < 0) {
        int err = -errno;
        free(nova);
        return err;
    }
    nova->io.version = SND_PCM_IOPLUG_VERSION;
    nova->io.name = "Nova OS mixer";
    nova->io.callback = &callbacks;
    nova->io.private_data = nova;
    nova->io.poll_fd = nova->fd;
    nova->io.poll_events = POLLOUT;
    nova->io.mmap_rw = 0;

    int err = snd_pcm_ioplug_create(&nova->io, name, stream, mode);
    if (err < 0) {
        close(nova->fd);
        free(nova);
        return err;
    }
    if ((err = constrain(&nova->io)) < 0) {
        snd_pcm_ioplug_delete(&nova->io);
        return err;
    }
    *pcmp = nova->io.pcm;
    return 0;
}

SND_PCM_PLUGIN_SYMBOL(nova);
