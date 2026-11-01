#!/usr/bin/env python3
"""Optional ROS test source: a moving arm plus an odom -> base_link transform."""
import math
from pathlib import Path
import time

import rclpy
from rclpy.qos import DurabilityPolicy, QoSProfile
from sensor_msgs.msg import JointState
from std_msgs.msg import String
from geometry_msgs.msg import TransformStamped
from tf2_msgs.msg import TFMessage

rclpy.init()
node = rclpy.create_node('reflecdith_demo')
description = node.create_publisher(
    String, '/robot_description',
    QoSProfile(depth=1, durability=DurabilityPolicy.TRANSIENT_LOCAL),
)
joints = node.create_publisher(JointState, '/joint_states', 10)
tf = node.create_publisher(TFMessage, '/tf', 10)
description.publish(String(data=Path(__file__).with_name('arm.urdf').read_text()))
start = time.monotonic()


def tick():
    t = time.monotonic() - start
    stamp = node.get_clock().now().to_msg()
    msg = JointState()
    msg.header.stamp = stamp
    msg.name = ['shoulder', 'elbow']
    msg.position = [0.7 * math.sin(t), 0.9 * math.cos(t * 0.8)]
    joints.publish(msg)
    pose = TransformStamped()
    pose.header.stamp = stamp
    pose.header.frame_id = 'odom'
    pose.child_frame_id = 'base_link'
    pose.transform.translation.x = 0.2 * math.sin(t * 0.4)
    pose.transform.rotation.z = math.sin(0.2 * math.sin(t * 0.3))
    pose.transform.rotation.w = math.cos(0.2 * math.sin(t * 0.3))
    tf.publish(TFMessage(transforms=[pose]))


node.create_timer(1 / 30, tick)
try:
    rclpy.spin(node)
except KeyboardInterrupt:
    pass
finally:
    node.destroy_node()
    rclpy.try_shutdown()
