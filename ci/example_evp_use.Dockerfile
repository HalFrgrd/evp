# syntax=docker/dockerfile:1.7
ARG BASE_IMAGE
FROM ${BASE_IMAGE} AS runner

# Write a small script to a file
RUN cat <<'EOF' > /root/test_run.tape
Output /root/test_run.gif

Set Shell bash
Set Width 800
Set Height 200
Set FontSize 18
Set TypingSpeed 50ms
Set Framerate 30

Sleep 1s
Type "echo 'Validating evp container!'"
Sleep 200ms
Enter
Sleep 1s
EOF

# Run evp to render the gif
RUN evp /root/test_run.tape --output /root/test_run.gif

# Export stage to extract the gif
FROM scratch AS export
COPY --from=runner /root/test_run.gif /
