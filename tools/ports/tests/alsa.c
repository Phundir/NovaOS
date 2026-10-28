/* Sound path of the Linux subsystem (phase 11.11): ALSA's "default"
 * device, i.e. /etc/asound.conf -> plug -> the "nova" plugin ->
 * /dev/nova-audio -> Nova's mixer. Plays half a second of a 440 Hz tone
 * in the format the Web Browser uses (float, 44.1 kHz), which the plug
 * layer converts. Exit 0 when every frame was accepted. */
#include <alsa/asoundlib.h>
#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

/* "muet": the same path, with silence (the boot self-test). */
static float amplitude = 0.3f;

/* What Firefox's cubeb asks before it opens a stream: a PCM set for a
 * latency of 100 frames, then the channel range of the device. A failure
 * here and Firefox plays nothing ("Output number of channels is 0"). */
static int probe(void) {
    snd_pcm_t *pcm;
    snd_pcm_hw_params_t *hw;
    unsigned int max = 0;
    int err = snd_pcm_open(&pcm, "default", SND_PCM_STREAM_PLAYBACK, SND_PCM_NONBLOCK);
    if (err == -ENODEV) { printf("alsa: aucune carte son - peripherique declare absent\n"); return -1; }
    if (err < 0) { printf("alsa: sonde, ouverture : %s\n", snd_strerror(err)); return 4; }
    err = snd_pcm_set_params(pcm, SND_PCM_FORMAT_FLOAT_LE, SND_PCM_ACCESS_RW_INTERLEAVED, 2, 44100, 1,
                             100 * 1000000 / 44100);
    if (err < 0) { printf("alsa: sonde, parametres (latence 100 trames) : %s\n", snd_strerror(err)); return 5; }
    snd_pcm_hw_params_alloca(&hw);
    err = snd_pcm_hw_params_any(pcm, hw);
    if (err < 0) { printf("alsa: sonde, hw_params_any : %s\n", snd_strerror(err)); return 6; }
    err = snd_pcm_hw_params_get_channels_max(hw, &max);
    if (err < 0) { printf("alsa: sonde, canaux max : %s\n", snd_strerror(err)); return 7; }
    printf("alsa: sonde de Firefox conforme - %u canaux au plus\n", max);
    snd_pcm_close(pcm);
    return 0;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc > 1 && strcmp(argv[1], "muet") == 0)
        amplitude = 0;
    int r = probe();
    if (r < 0) return 0;
    if (r) return r;
    snd_pcm_t *pcm;
    int err = snd_pcm_open(&pcm, "default", SND_PCM_STREAM_PLAYBACK, 0);
    if (err < 0) { printf("alsa: ouverture : %s\n", snd_strerror(err)); return 1; }
    err = snd_pcm_set_params(pcm, SND_PCM_FORMAT_FLOAT_LE, SND_PCM_ACCESS_RW_INTERLEAVED, 2, 44100, 1, 100000);
    if (err < 0) { printf("alsa: parametres : %s\n", snd_strerror(err)); return 2; }
    static float frames[22050 * 2];
    for (int i = 0; i < 22050; i++)
        frames[2 * i] = frames[2 * i + 1] = amplitude * sinf(2 * 3.14159265f * 440 * i / 44100);
    snd_pcm_sframes_t done = 0;
    while (done < 22050) {
        snd_pcm_sframes_t n = snd_pcm_writei(pcm, frames + 2 * done, 22050 - done);
        if (n < 0) n = snd_pcm_recover(pcm, n, 0);
        if (n < 0) { printf("alsa: ecriture : %s\n", snd_strerror(n)); return 3; }
        done += n;
    }
    printf("alsa: %ld trames ecrites, vidage\n", (long)done);
    snd_pcm_drain(pcm);
    snd_pcm_close(pcm);
    printf("alsa: %ld trames jouees par le peripherique \"default\"\n", (long)done);
    return 0;
}
