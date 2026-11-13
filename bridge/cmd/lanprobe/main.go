// Temporary diagnostic: one LAN attempt, times handshake and first frame.
package main

import (
	"context"
	"fmt"
	"os"
	"sync/atomic"
	"time"

	"avent-webrtc-bridge/pkg/lan"
)

func main() {
	t0 := time.Now()
	el := func() string { return fmt.Sprintf("+%5.1fs", time.Since(t0).Seconds()) }
	var fr, au atomic.Uint64
	first := make(chan struct{}, 1)
	c := lan.NewClient(lan.ClientConfig{
		DeviceID: os.Getenv("AVENT_DEVICE_ID"), LocalKey: os.Getenv("AVENT_LOCAL_KEY"),
		Password: os.Getenv("AVENT_P2P_PASSWORD"), UID: os.Getenv("AVENT_DEVICE_ID"),
		IP: "192.168.1.46", Version: 3.3, HandshakeTimeout: 30 * time.Second,
		OnVideo: func(f *lan.VideoFrame) {
			if fr.Add(1) == 1 {
				fmt.Printf("%s FIRST VIDEO FRAME\n", el())
				select {
				case first <- struct{}{}:
				default:
				}
			}
		},
		OnAudio:  func(f *lan.AudioFrame) { au.Add(1) },
		OnClosed: func(e error) { fmt.Printf("%s OnClosed: %v\n", el(), e) },
	})
	ctx, cancel := context.WithTimeout(context.Background(), 35*time.Second)
	defer cancel()
	fmt.Printf("%s dialing 192.168.1.46\n", el())
	if err := c.Start(ctx); err != nil {
		fmt.Printf("%s Start FAILED: %v\n", el(), err)
		os.Exit(1)
	}
	fmt.Printf("%s handshake OK, waiting up to 60s for video\n", el())
	select {
	case <-first:
		time.Sleep(3 * time.Second)
		fmt.Printf("%s VIDEO FLOWS: %d video, %d audio\n", el(), fr.Load(), au.Load())
	case <-time.After(60 * time.Second):
		fmt.Printf("%s NO video in 60s\n", el())
	}
	_ = c.Close()
}
